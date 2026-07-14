use super::*;

fn merge_account_rate_limits(runtime: &crate::RuntimePaths, kind: &str, windows: AgentRateLimits) {
    super::merge_account_rate_limits(runtime, kind, Default::default(), windows);
}
use crate::ids::WorkspaceId;
use crate::sidebar::test_support::{
    provider_panel, rl_window, rl_window_mins, snapshot_with_panels,
};
use jiff::SignedDuration;

fn authoritative(mut window: RateLimitWindow) -> RateLimitWindow {
    window.source = WindowSource::Authoritative;
    window
}

fn kind_wide_cache(
    refreshed_at_ms: u64,
    windows: BTreeMap<String, AgentRateLimits>,
    mut pending: BTreeMap<String, Vec<PendingRefill>>,
) -> RateLimitsCache {
    let mut entries = windows
        .into_iter()
        .map(|(kind, limits)| {
            let pending = pending.remove(&kind).unwrap_or_default();
            (
                kind,
                crate::agents::RateLimitCacheEntry {
                    scope: Default::default(),
                    limits,
                    pending,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    entries.extend(pending.into_iter().map(|(kind, pending)| {
        (
            kind,
            crate::agents::RateLimitCacheEntry {
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

fn cache_limits<'a>(cache: &'a RateLimitsCache, kind: &str) -> &'a AgentRateLimits {
    &cache.entries[kind].limits
}

#[test]
fn idle_window_projection_ages_only_known_elapsed_windows() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let future = Timestamp::from_second(2_000_010_000).unwrap();
    let cached = rl_window(80, Some(future));
    let projected = project_window(cached.clone(), now);
    assert_eq!(projected, cached, "before reset the cached reading stands");

    let passed = Timestamp::from_second(1_999_990_000).unwrap();
    let projected = project_window(rl_window(95, Some(passed)), now);
    assert_eq!(projected.used_percentage, Some(0), "a reset window is full");
    assert_eq!(
        projected.resets_at,
        now.checked_add(SignedDuration::from_secs(300 * 60)).ok(),
        "the reset rolls one window length (300 min) forward from now"
    );

    let undated = rl_window(40, None);
    assert_eq!(project_window(undated.clone(), now), undated);
    let no_duration = RateLimitWindow {
        used_percentage: Some(90),
        resets_at: Some(passed),
        duration_mins: None,
        ..Default::default()
    };
    assert_eq!(project_window(no_duration.clone(), now), no_duration);
}

fn scoped_window(id: &str, label: &str, used: u8, reset: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        scope: Some(crate::agents::RateLimitWindowScope {
            id: id.to_owned(),
            label: label.to_owned(),
        }),
        used_percentage: Some(used),
        resets_at: Some(reset),
        duration_mins: None,
        source: WindowSource::Authoritative,
        ..Default::default()
    }
}

#[test]
fn scoped_windows_fuse_and_round_trip_independently() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let reset = Timestamp::from_second(4_000_000_000).unwrap();
    let mut snapshot = snapshot_with_panels(
        workspace,
        vec![provider_panel(
            "copilot",
            vec![
                scoped_window("premium_interactions", "prm", 20, reset),
                scoped_window("chat", "cht", 70, reset),
            ],
        )],
    );
    apply_rate_limit_cache(&mut snapshot, &runtime, true);

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let windows = &cache.entries["copilot"].limits.windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(
        windows
            .iter()
            .find(|window| window
                .scope
                .as_ref()
                .is_some_and(|scope| scope.id == "premium_interactions"))
            .and_then(|window| window.used_percentage),
        Some(20)
    );
    assert_eq!(
        windows
            .iter()
            .find(|window| window
                .scope
                .as_ref()
                .is_some_and(|scope| scope.id == "chat"))
            .and_then(|window| window.used_percentage),
        Some(70)
    );

    let encoded = serde_json::to_vec(&cache).unwrap();
    let decoded: RateLimitsCache = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.entries["copilot"].limits.windows, *windows);
}

#[test]
fn expired_durationless_scoped_cache_displays_unknown_independently() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &RateLimitsCache {
            refreshed_at_ms: 1,
            entries: BTreeMap::from([(
                "copilot".to_owned(),
                RateLimitCacheEntry {
                    scope: Default::default(),
                    limits: AgentRateLimits {
                        windows: vec![
                            scoped_window("premium_interactions", "prm", 100, passed),
                            scoped_window("chat", "cht", 40, future),
                        ],
                    },
                    pending: Vec::new(),
                },
            )]),
            ..Default::default()
        },
    );
    let mut snapshot = snapshot_with_panels(workspace, vec![provider_panel("copilot", Vec::new())]);
    apply_rate_limit_cache(&mut snapshot, &runtime, false);

    assert_eq!(snapshot.providers[0].windows.len(), 2);
    let premium = &snapshot.providers[0].windows[1];
    assert_eq!(
        premium.scope.as_ref().map(|scope| scope.label.as_str()),
        Some("prm")
    );
    assert_eq!(premium.used_percentage, None);
    assert_eq!(premium.resets_at, None);
    let chat = &snapshot.providers[0].windows[0];
    assert_eq!(
        chat.scope.as_ref().map(|scope| scope.label.as_str()),
        Some("cht")
    );
    assert_eq!(chat.used_percentage, Some(40));
    assert_eq!(chat.resets_at, Some(future));
}

#[test]
fn pre_scope_cache_schema_is_cold_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rate_limits.json");
    std::fs::write(
        &path,
        r#"{"refreshed_at_ms":1,"windows":{"qwen":{"windows":[]}},"pending":{}}"#,
    )
    .unwrap();
    let cache = read_rate_limits_cache(&path);
    assert!(cache.entries.is_empty());
    assert_eq!(cache.version, RateLimitsCache::default().version);
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
        cache_limits(&cache, "claude")
            .windows
            .first()
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

#[test]
fn scoped_windows_render_only_for_the_matching_provider_region() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_rate_limits_path();
    let international =
        crate::agents::ProviderAccountScope::sub_provider("alibaba", "international");
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            entries: BTreeMap::from([(
                "qwen".to_owned(),
                crate::agents::RateLimitCacheEntry {
                    scope: international.clone(),
                    limits: AgentRateLimits {
                        windows: vec![
                            rl_window_mins(20, None, 300),
                            rl_window_mins(40, None, 10_080),
                            rl_window_mins(60, None, 43_200),
                        ],
                    },
                    pending: Vec::new(),
                },
            )]),
            ..Default::default()
        },
    );

    let mut matching =
        snapshot_with_panels(workspace.clone(), vec![provider_panel("qwen", Vec::new())]);
    matching.providers[0].account_scope = international;
    apply_rate_limit_cache(&mut matching, &runtime, false);
    assert_eq!(
        matching.providers[0]
            .windows
            .iter()
            .map(|window| window.duration_mins)
            .collect::<Vec<_>>(),
        [Some(300), Some(10_080), Some(43_200)]
    );

    let mut china = snapshot_with_panels(workspace, vec![provider_panel("qwen", Vec::new())]);
    china.providers[0].account_scope =
        crate::agents::ProviderAccountScope::sub_provider("alibaba", "china");
    apply_rate_limit_cache(&mut china, &runtime, true);
    assert!(china.providers[0].windows.is_empty());
    assert!(read_rate_limits_cache(&path).entries.is_empty());
}

#[test]
fn producer_reset_advance_invalidates_oauth_usage_throttle() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
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
        },
    );
    let marker = crate::sidebar::refresh::usage::usage_probe_marker(&runtime, "codex");
    std::fs::write(&marker, b"").unwrap();

    let mut frame = snapshot_with_panels(
        workspace,
        vec![provider_panel("codex", vec![rl_window(1, Some(new_reset))])],
    );
    apply_rate_limit_cache(&mut frame, &runtime, true);

    assert!(
        !marker.exists(),
        "new budget epoch removes the producer spawn throttle"
    );
    assert_eq!(
        crate::sidebar::refresh::credits::read_credits_cache(&runtime.shared_credits_path())
            .entries
            .get("codex")
            .map(|entry| entry.oauth_read_at_ms),
        Some(0),
        "new budget epoch clears the helper-side OAuth gate"
    );
}

#[test]
fn producer_write_then_reader_merge_is_value_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();

    let mut writer = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel(
            "claude",
            vec![
                rl_window_mins(60, Some(future), 300),
                rl_window_mins(35, Some(future), 7 * 24 * 60),
            ],
        )],
    );
    let mut reader = writer.clone();

    apply_rate_limit_cache(&mut writer, &runtime, true);
    apply_rate_limit_cache(&mut reader, &runtime, false);

    assert_eq!(
        reader.providers[0].windows, writer.providers[0].windows,
        "a reader merge after the producer write preserves the panel values"
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
        &kind_wide_cache(
            0,
            BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        rl_window_mins(90, Some(passed), 300),
                        rl_window_mins(70, Some(future), 7 * 24 * 60),
                    ],
                },
            )]),
            BTreeMap::new(),
        ),
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
        cache_limits(&persisted, "claude")
            .windows
            .first()
            .and_then(|window| window.used_percentage),
        Some(90),
        "the cache retains ground truth, not the synthesized full window"
    );
}

/// A live reading can carry a longer window whose own reset has already passed.
/// The shorter window is still future, so the reading survives upstream; display
/// rolls the expired longer window forward instead of freezing its countdown at
/// `0h00m`, while persisted truth stays raw.
#[test]
fn live_reading_with_expired_longer_window_rolls_forward() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap(); // 2001 — always past
    let future = Timestamp::from_second(4_000_000_000).unwrap(); // 2096 — always future

    let mut frame = snapshot_with_panels(
        workspace,
        vec![provider_panel(
            "claude",
            vec![
                rl_window_mins(40, Some(future), 300),
                rl_window_mins(80, Some(passed), 7 * 24 * 60),
            ],
        )],
    );
    apply_rate_limit_cache(&mut frame, &runtime, true);

    let shown = &frame.providers[0].windows;
    assert_eq!(
        shown[0].used_percentage,
        Some(40),
        "the live 5h window is unchanged"
    );
    assert_eq!(shown[0].resets_at, Some(future));
    assert_eq!(
        shown[1].used_percentage,
        Some(0),
        "the expired 7d window rolls to full"
    );
    assert!(
        shown[1].resets_at.is_some_and(|reset| reset > frame.now),
        "with a future, rolled-forward countdown — not a frozen 0h00m"
    );

    let persisted = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let persisted_7d = cache_limits(&persisted, "claude")
        .windows
        .iter()
        .find(|window| window.duration_mins == Some(7 * 24 * 60))
        .expect("the 7d window is persisted");
    assert_eq!(
        persisted_7d.used_percentage,
        Some(80),
        "ground truth stays raw"
    );
    assert_eq!(persisted_7d.resets_at, Some(passed));
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
        &kind_wide_cache(
            0,
            BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        rl_window_mins(90, Some(passed), 300),
                        rl_window_mins(80, Some(passed), 7 * 24 * 60),
                    ],
                },
            )]),
            BTreeMap::new(),
        ),
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
    let persisted_windows = &cache_limits(&persisted, "claude").windows;
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
    assert!(seeded_cache.entries.contains_key("claude"));
    assert!(seeded_cache.entries.contains_key("codex"));

    // Codex logs out: only claude has a panel now. The next producer write
    // rebuilds the cache from the surviving panels, so codex drops out while
    // claude's windows are kept.
    let mut codex_gone =
        snapshot_with_panels(workspace, vec![provider_panel("claude", windows(40))]);
    apply_rate_limit_cache(&mut codex_gone, &runtime, true);
    let after = read_rate_limits_cache(&path);
    assert!(
        after.entries.contains_key("claude"),
        "a still-logged-in provider keeps its windows"
    );
    assert!(
        !after.entries.contains_key("codex"),
        "a logged-out provider's windows drop on the next write"
    );
}

/// When the *last* provider logs out there is no surviving panel to rebuild the
/// cache from, so the producer reaps it wholesale — a later re-login paints from
/// live readings, never stale budgets. A consumer never touches it.
#[test]
fn producer_reaps_cache_when_every_provider_logs_out() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let path = runtime.shared_rate_limits_path();
    let seed = || {
        write_rate_limits_cache(
            &path,
            &kind_wide_cache(
                1,
                BTreeMap::from([(
                    "claude".to_owned(),
                    AgentRateLimits {
                        windows: vec![rl_window(40, Some(future))],
                    },
                )]),
                BTreeMap::new(),
            ),
        );
    };

    // A consumer (persist=false) with no panels leaves the cache intact.
    seed();
    let mut consumer = snapshot_with_panels(workspace.clone(), Vec::new());
    apply_rate_limit_cache(&mut consumer, &runtime, false);
    assert!(
        read_rate_limits_cache(&path).entries.contains_key("claude"),
        "a consumer never reaps the cache"
    );

    // The producer (persist=true) with no panels clears it.
    let mut producer = snapshot_with_panels(workspace, Vec::new());
    apply_rate_limit_cache(&mut producer, &runtime, true);
    assert!(
        read_rate_limits_cache(&path).entries.is_empty(),
        "a fully logged-out room reaps its stale windows so a re-login can't flash them"
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
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(20, None)],
                },
            )]),
            BTreeMap::new(),
        ),
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
        cache_limits(&cache, "codex")
            .windows
            .first()
            .and_then(|w| w.used_percentage),
        Some(55),
        "the idle provider's windows are seeded"
    );
    assert!(
        cache.entries.contains_key("claude"),
        "an existing kind's windows are preserved"
    );
}

#[test]
fn authoritative_merge_marks_omitted_windows_lifted_until_reported_again() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_rate_limits_path();
    let five_hours = 5 * 60;
    let seven_days = 7 * 24 * 60;

    write_rate_limits_cache(
        &path,
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "codex".to_owned(),
                AgentRateLimits {
                    windows: vec![
                        rl_window_mins(20, None, five_hours),
                        rl_window_mins(40, None, seven_days),
                    ],
                },
            )]),
            BTreeMap::new(),
        ),
    );

    let only_week = AgentRateLimits {
        windows: vec![authoritative(rl_window_mins(41, None, seven_days))],
    };
    merge_account_rate_limits(&runtime, "codex", only_week.clone());
    merge_account_rate_limits(&runtime, "codex", only_week);

    let cache = read_rate_limits_cache(&path);
    let windows = &cache_limits(&cache, "codex").windows;
    assert_eq!(windows.len(), 2, "re-merging the omission is idempotent");
    let lifted = windows
        .iter()
        .find(|window| window.duration_mins == Some(five_hours))
        .expect("the omitted 5h window remains visible");
    assert!(lifted.lifted);
    assert_eq!(lifted.used_percentage, None);
    assert_eq!(lifted.resets_at, None);
    assert_eq!(lifted.source, WindowSource::Authoritative);
    assert!(lifted.observed_at.is_some());
    assert!(
        windows
            .iter()
            .find(|window| window.duration_mins == Some(seven_days))
            .is_some_and(|window| !window.lifted && window.used_percentage == Some(41)),
        "the reported 7d window stays a real reading"
    );

    merge_account_rate_limits(
        &runtime,
        "codex",
        AgentRateLimits {
            windows: vec![
                authoritative(rl_window_mins(2, None, five_hours)),
                authoritative(rl_window_mins(42, None, seven_days)),
            ],
        },
    );
    let cache = read_rate_limits_cache(&path);
    assert!(
        cache_limits(&cache, "codex")
            .windows
            .iter()
            .all(|window| !window.lifted),
        "reporting the duration again clears the lift"
    );

    merge_account_rate_limits(&runtime, "codex", AgentRateLimits::default());
    assert!(
        cache_limits(&read_rate_limits_cache(&path), "codex")
            .windows
            .is_empty(),
        "an empty reading does not infer lifted windows"
    );
}

#[test]
fn authoritative_merge_does_not_fabricate_lifted_named_quotas() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let reset = Timestamp::from_second(4_000_000_000).unwrap();

    merge_account_rate_limits(
        &runtime,
        "copilot",
        AgentRateLimits {
            windows: vec![
                scoped_window("premium_interactions", "prm", 20, reset),
                scoped_window("chat", "cht", 40, reset),
            ],
        },
    );
    merge_account_rate_limits(
        &runtime,
        "copilot",
        AgentRateLimits {
            windows: vec![scoped_window("chat", "cht", 41, reset)],
        },
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let windows = &cache.entries["copilot"].limits.windows;
    assert_eq!(windows.len(), 1);
    assert_eq!(
        windows[0].scope.as_ref().map(|scope| scope.id.as_str()),
        Some("chat")
    );
    assert!(!windows[0].lifted);
}

#[test]
fn codex_cold_cache_synthesizes_declared_omitted_window_as_lifted() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let seven_days = 7 * 24 * 60;

    merge_account_rate_limits(
        &runtime,
        "codex",
        AgentRateLimits {
            windows: vec![authoritative(rl_window_mins(41, None, seven_days))],
        },
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let windows = &cache_limits(&cache, "codex").windows;
    assert_eq!(windows.len(), 2);
    let five_hours = windows
        .iter()
        .find(|window| window.duration_mins == Some(5 * 60))
        .expect("the declared 5h window is synthesized from a cold cache");
    assert!(five_hours.lifted);
    assert_eq!(five_hours.used_percentage, None);
    assert_eq!(five_hours.source, WindowSource::Authoritative);
    assert!(
        windows
            .iter()
            .find(|window| window.duration_mins == Some(seven_days))
            .is_some_and(|window| !window.lifted && window.used_percentage == Some(41))
    );
}

#[test]
fn codex_reported_declared_window_stays_real() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    merge_account_rate_limits(
        &runtime,
        "codex",
        AgentRateLimits {
            windows: vec![
                authoritative(rl_window_mins(12, None, 5 * 60)),
                authoritative(rl_window_mins(41, None, 7 * 24 * 60)),
            ],
        },
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let windows = &cache_limits(&cache, "codex").windows;
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().all(|window| !window.lifted));
    assert!(
        windows
            .iter()
            .find(|window| window.duration_mins == Some(5 * 60))
            .is_some_and(|window| window.used_percentage == Some(12))
    );
}

#[test]
fn authoritative_live_panel_completes_and_persists_codex_omission_in_one_frame() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let observed_at = Timestamp::now();
    let seven_days = 7 * 24 * 60;
    let weekly = RateLimitWindow {
        observed_at: Some(observed_at),
        ..authoritative(rl_window_mins(41, None, seven_days))
    };
    let mut snapshot = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("codex", vec![weekly])],
    );

    apply_rate_limit_cache(&mut snapshot, &runtime, true);

    assert_eq!(snapshot.providers[0].windows.len(), 2);
    assert!(
        snapshot.providers[0]
            .windows
            .iter()
            .any(|window| window.duration_mins == Some(300) && window.lifted)
    );
    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    assert!(
        cache_limits(&cache, "codex")
            .windows
            .iter()
            .any(|window| window.duration_mins == Some(300) && window.lifted),
        "same-frame completion is persisted as fused truth"
    );

    let real = [(300, 2), (seven_days, 42)]
        .into_iter()
        .map(|(duration, used)| RateLimitWindow {
            observed_at: Some(Timestamp::now()),
            ..authoritative(rl_window_mins(used, None, duration))
        })
        .collect();
    let mut reported = snapshot_with_panels(workspace, vec![provider_panel("codex", real)]);
    apply_rate_limit_cache(&mut reported, &runtime, true);
    assert!(
        reported.providers[0]
            .windows
            .iter()
            .all(|window| !window.lifted)
    );
    assert!(
        cache_limits(
            &read_rate_limits_cache(&runtime.shared_rate_limits_path()),
            "codex",
        )
        .windows
        .iter()
        .all(|window| !window.lifted),
        "a real 5h reading replaces the persisted lifted row"
    );
}

#[test]
fn completion_requires_authoritative_scope_applicable_temporal_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let observed_at = Timestamp::now();
    let seven_days = 7 * 24 * 60;
    let weekly = |source| RateLimitWindow {
        observed_at: Some(observed_at),
        source,
        ..rl_window_mins(41, None, seven_days)
    };

    let mut best_effort = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel(
            "codex",
            vec![weekly(WindowSource::BestEffort)],
        )],
    );
    apply_rate_limit_cache(&mut best_effort, &runtime, false);
    assert_eq!(best_effort.providers[0].windows.len(), 1);

    for kind in ["pi", "opencode"] {
        let mut openai_panel = provider_panel(kind, vec![weekly(WindowSource::Authoritative)]);
        openai_panel.account_scope = ProviderAccountScope::sub_provider("openai", "oauth");
        let mut openai = snapshot_with_panels(workspace.clone(), vec![openai_panel]);
        apply_rate_limit_cache(&mut openai, &runtime, false);
        assert_eq!(openai.providers[0].windows.len(), 2, "{kind} OpenAI");

        let mut anthropic_panel = provider_panel(kind, vec![weekly(WindowSource::Authoritative)]);
        anthropic_panel.account_scope = ProviderAccountScope::sub_provider("anthropic", "oauth");
        let mut anthropic = snapshot_with_panels(workspace.clone(), vec![anthropic_panel]);
        apply_rate_limit_cache(&mut anthropic, &runtime, false);
        assert_eq!(anthropic.providers[0].windows.len(), 1, "{kind} Anthropic");
    }

    let mut openai_seed = provider_panel("pi", vec![weekly(WindowSource::Authoritative)]);
    openai_seed.account_scope = ProviderAccountScope::sub_provider("openai", "oauth");
    let mut seeded = snapshot_with_panels(workspace.clone(), vec![openai_seed]);
    apply_rate_limit_cache(&mut seeded, &runtime, true);
    let mut switched_panel = provider_panel("pi", vec![weekly(WindowSource::Authoritative)]);
    switched_panel.account_scope = ProviderAccountScope::sub_provider("anthropic", "oauth");
    let mut switched = snapshot_with_panels(workspace, vec![switched_panel]);
    apply_rate_limit_cache(&mut switched, &runtime, false);
    assert_eq!(
        switched.providers[0].windows.len(),
        1,
        "same-kind prior windows from another scope are excluded"
    );
}

#[test]
fn undeclared_provider_cold_cache_does_not_synthesize_window() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    merge_account_rate_limits(
        &runtime,
        "claude",
        AgentRateLimits {
            windows: vec![rl_window_mins(41, None, 7 * 24 * 60)],
        },
    );

    let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
    let windows = &cache_limits(&cache, "claude").windows;
    assert_eq!(windows.len(), 1);
    assert!(!windows[0].lifted);
}

#[test]
fn drop_kind_rate_limits_removes_only_that_kinds_windows_and_pending() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_rate_limits_path();
    let first_seen_at = Timestamp::from_second(2_000_000_000).unwrap();
    write_rate_limits_cache(
        &path,
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
                (
                    "codex".to_owned(),
                    vec![PendingRefill {
                        scope_id: None,
                        duration_mins: Some(300),
                        used_percentage: 1,
                        first_seen_at,
                    }],
                ),
                (
                    "claude".to_owned(),
                    vec![PendingRefill {
                        scope_id: None,
                        duration_mins: Some(300),
                        used_percentage: 2,
                        first_seen_at,
                    }],
                ),
            ]),
        ),
    );

    drop_kind_rate_limits(&runtime, "codex");
    let cache = read_rate_limits_cache(&path);

    assert!(!cache.entries.contains_key("codex"));
    assert!(cache.entries.contains_key("claude"));
    assert!(!cache.entries["claude"].pending.is_empty());

    let refreshed_at_ms = cache.refreshed_at_ms;
    drop_kind_rate_limits(&runtime, "missing");
    assert_eq!(
        read_rate_limits_cache(&path).refreshed_at_ms,
        refreshed_at_ms,
        "absent kind is a no-op"
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
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(20, None)],
                },
            )]),
            BTreeMap::new(),
        ),
    );

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

    let cache = read_rate_limits_cache(&path);
    assert_eq!(
        cache_limits(&cache, "claude")
            .windows
            .first()
            .and_then(|window| window.used_percentage),
        Some(20),
        "a producer that cannot get the RMW lock leaves existing kinds intact"
    );
    assert!(
        !cache.entries.contains_key("codex"),
        "the contending producer does not publish its partial provider set"
    );
    lock_file.unlock().unwrap();
}

#[test]
fn detached_authoritative_merge_waits_for_lock_and_preserves_other_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.shared_rate_limits_path();
    write_rate_limits_cache(
        &path,
        &kind_wide_cache(
            1,
            BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(20, None)],
                },
            )]),
            BTreeMap::new(),
        ),
    );
    let held =
        crate::store::lock::WorkspaceLock::acquire(&runtime.shared_rate_limits_lock()).unwrap();
    let worker_runtime = runtime.clone();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        merge_account_rate_limits(
            &worker_runtime,
            "codex",
            AgentRateLimits {
                windows: vec![authoritative(rl_window(55, None))],
            },
        );
        finished_tx.send(()).unwrap();
    });
    assert!(
        finished_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "the detached merge waits instead of discarding a fetched observation"
    );

    drop(held);
    finished_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    worker.join().unwrap();
    let cache = read_rate_limits_cache(&path);
    assert!(cache.entries.contains_key("claude"));
    assert!(cache.entries.contains_key("codex"));
}

// ── fuse_window: source- and time-aware refill trust ────────────────────────

use crate::agents::context::WindowSource;
fn fuse_now() -> Timestamp {
    Timestamp::from_second(2_000_000_000).unwrap()
}

/// A best-effort (statusline) window reading.
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

/// An authoritative (official-API) window reading.
fn auth(used: u8, resets_at: Timestamp, observed_at: Timestamp) -> RateLimitWindow {
    RateLimitWindow {
        source: WindowSource::Authoritative,
        ..be(used, resets_at, observed_at)
    }
}

#[test]
fn fuse_climb_is_immediate_and_clears_any_pending() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let parked = PendingRefill {
        scope_id: None,
        duration_mins: Some(300),
        used_percentage: 2,
        first_seen_at: now,
    };
    // A reading at or above the prior is real consumption — adopt at once.
    let (truth, pending) = fuse_window(
        Some(&be(20, reset, now)),
        Some(&be(35, reset, now)),
        Some(&parked),
        now,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(35));
    assert!(pending.is_none(), "a climb cancels a parked refill");
}

#[test]
fn fuse_authoritative_drop_is_immediate() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let (truth, pending) = fuse_window(
        Some(&be(80, reset, now)),
        Some(&auth(2, reset, now)),
        None,
        now,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(2),
        "an official reading lowers the bar now"
    );
    assert!(pending.is_none());
}

#[test]
fn fuse_carries_a_lifted_window_until_a_real_reading_replaces_it() {
    let now = fuse_now();
    let lifted = RateLimitWindow {
        duration_mins: Some(300),
        observed_at: Some(now),
        source: WindowSource::Authoritative,
        lifted: true,
        ..Default::default()
    };

    let (carried, pending) = fuse_window(Some(&lifted), None, None, now, true);
    assert_eq!(carried.as_ref(), Some(&lifted));
    assert!(pending.is_none());

    let reset = now + SignedDuration::from_secs(3_600);
    let live = auth(1, reset, now);
    let (replaced, pending) = fuse_window(Some(&lifted), Some(&live), None, now, true);
    assert_eq!(replaced.as_ref(), Some(&live));
    assert!(!replaced.expect("a real reading").lifted);
    assert!(pending.is_none());
}

#[test]
fn fuse_reset_timer_advance_accepts_the_drop() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let later = reset + SignedDuration::from_secs(7_200);
    let (truth, _) = fuse_window(
        Some(&be(80, reset, now)),
        Some(&be(1, later, now)),
        None,
        now,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(1),
        "a later reset instant proves a new window epoch and lowers at once"
    );
}

#[test]
fn fuse_best_effort_refill_holds_then_confirms() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    // First sight of a same-epoch best-effort drop: hold the higher bar, park it.
    let (truth, pending) = fuse_window(
        Some(&be(75, reset, now)),
        Some(&be(1, reset, now)),
        None,
        now,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(75),
        "held on first sight"
    );
    let parked = pending.expect("a refill candidate is parked");
    assert_eq!(parked.used_percentage, 1);

    // Still within the confirm window: still held.
    let within = now + SignedDuration::from_secs(REFILL_CONFIRM_SECS - 1);
    let (truth, still) = fuse_window(
        Some(&be(75, reset, within)),
        Some(&be(2, reset, within)),
        Some(&parked),
        within,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(75),
        "still held before confirm elapses"
    );
    assert!(still.is_some());

    // Past the confirm window: the sustained refill is adopted.
    let after = now + SignedDuration::from_secs(REFILL_CONFIRM_SECS + 1);
    let (truth, done) = fuse_window(
        Some(&be(75, reset, after)),
        Some(&be(3, reset, after)),
        Some(&parked),
        after,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(3),
        "the confirmed refill follows the live reading"
    );
    assert!(done.is_none());
}

#[test]
fn fuse_transient_low_then_climb_never_dips() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let (_, pending) = fuse_window(
        Some(&be(40, reset, now)),
        Some(&be(2, reset, now)),
        None,
        now,
        true,
    );
    assert!(pending.is_some(), "the low sample is parked, not shown");

    let next = now + SignedDuration::from_secs(5);
    let (truth, pending) = fuse_window(
        Some(&be(40, reset, next)),
        Some(&be(41, reset, next)),
        pending.as_ref(),
        next,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(41), "the climb wins");
    assert!(pending.is_none(), "one stray low sample never dips the bar");
}

#[test]
fn fuse_consumer_never_lowers_the_bar_on_its_own() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let (truth, _) = fuse_window(
        Some(&be(75, reset, now)),
        Some(&be(1, reset, now)),
        None,
        now,
        false,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(75),
        "a consumer mirrors the producer's persisted truth"
    );
}

#[test]
fn fuse_ignores_a_wildly_old_live_reading() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let old = now - SignedDuration::from_secs(LIVE_HORIZON_SECS + 1);
    let (truth, _) = fuse_window(
        Some(&be(50, reset, now)),
        Some(&be(2, reset, old)),
        None,
        now,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(50),
        "a stale capture can't move the bar"
    );
}

#[test]
fn fuse_stale_authoritative_drop_cannot_lower_a_newer_bar() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    let older = now - SignedDuration::from_secs(60);
    // The prior is the newer authoritative truth (80% @ now); an out-of-order
    // sidecar reports 2% but its capture is a minute older. The newer bar holds.
    let (truth, pending) = fuse_window(
        Some(&auth(80, reset, now)),
        Some(&auth(2, reset, older)),
        None,
        now,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(80),
        "an older authoritative reading can't undo a newer one"
    );
    assert!(
        pending.is_none(),
        "a stale authoritative drop never seeds the best-effort debounce"
    );
}

#[test]
fn fuse_mid_range_best_effort_drop_holds_most_drained() {
    let now = fuse_now();
    let reset = now + SignedDuration::from_secs(3_600);
    // 80% -> 70% with the same reset: a mid-range best-effort drop is jitter,
    // not a refill. It holds the most-drained prior and is never parked.
    let (truth, pending) = fuse_window(
        Some(&be(80, reset, now)),
        Some(&be(70, reset, now)),
        None,
        now,
        true,
    );
    assert_eq!(
        truth.unwrap().used_percentage,
        Some(80),
        "a mid-range drop above the reset floor holds the most-drained prior"
    );
    assert!(
        pending.is_none(),
        "only a near-full refill candidate is parked for confirmation"
    );

    // And it never confirms with time: it was never parked, so the bar holds.
    let after = now + SignedDuration::from_secs(REFILL_CONFIRM_SECS + 1);
    let (truth, pending) = fuse_window(
        Some(&be(80, reset, after)),
        Some(&be(70, reset, after)),
        None,
        after,
        true,
    );
    assert_eq!(truth.unwrap().used_percentage, Some(80));
    assert!(pending.is_none());
}

// ── shortest_window_running: the window-priming ping guard ───────────────────

use crate::agents::{longest_window_reset_at, longest_window_running, shortest_window_running};

/// Seed `claude`'s windows into a fresh shared cache and report the ping guard's
/// verdict for `now`.
fn runtime_with_windows(windows: Vec<RateLimitWindow>) -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    write_rate_limits_cache(
        &runtime.shared_rate_limits_path(),
        &kind_wide_cache(
            0,
            BTreeMap::from([("claude".to_owned(), AgentRateLimits { windows })]),
            BTreeMap::new(),
        ),
    );
    (dir, runtime)
}

fn running_verdict(windows: Vec<RateLimitWindow>, now: Timestamp) -> Option<bool> {
    let (_dir, runtime) = runtime_with_windows(windows);
    shortest_window_running(&runtime, "claude", now)
}

fn longest_verdict(windows: Vec<RateLimitWindow>, now: Timestamp) -> Option<bool> {
    let (_dir, runtime) = runtime_with_windows(windows);
    longest_window_running(&runtime, "claude", now)
}

#[test]
fn ping_guard_skips_a_running_window_and_primes_an_idle_one() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let full_out = now
        .checked_add(SignedDuration::from_secs(300 * 60))
        .unwrap();
    let mid = now.checked_add(SignedDuration::from_secs(3600)).unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap();

    // A fresh window: ~1% used with the reset slid a full 5h out — not started.
    assert_eq!(
        running_verdict(vec![rl_window(1, Some(full_out))], now),
        Some(false),
        "a not-started window is primed"
    );
    // A live window counting down — already running, so the ping is skipped.
    assert_eq!(
        running_verdict(vec![rl_window(40, Some(mid))], now),
        Some(true),
        "a counting-down window is left alone"
    );
    // An idle window whose reset has passed projects to full — prime it again.
    assert_eq!(
        running_verdict(vec![rl_window(90, Some(passed))], now),
        Some(false),
        "a refilled idle window reads as not started"
    );
    // The shortest window decides: a not-started 5h under a running 7d still primes.
    assert_eq!(
        running_verdict(
            vec![
                rl_window_mins(1, Some(full_out), 300),
                rl_window_mins(50, Some(mid), 7 * 24 * 60),
            ],
            now,
        ),
        Some(false),
        "the shortest window drives the decision"
    );
}

#[test]
fn ping_guard_is_unknown_without_a_usable_reading() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    // No window to read — the caller defaults to priming.
    assert_eq!(
        running_verdict(Vec::new(), now),
        None,
        "no window means no verdict"
    );
    // A bar with no usage percentage carries no verdict either.
    let unknown = RateLimitWindow {
        used_percentage: None,
        resets_at: None,
        duration_mins: Some(300),
        ..Default::default()
    };
    assert_eq!(
        running_verdict(vec![unknown], now),
        None,
        "an unknown bar yields no verdict"
    );
}

#[test]
fn longest_ping_guard_uses_the_longest_window() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let five_hour_full = now
        .checked_add(SignedDuration::from_secs(300 * 60))
        .unwrap();
    let seven_day_full = now
        .checked_add(SignedDuration::from_secs(7 * 24 * 60 * 60))
        .unwrap();
    let mid = now.checked_add(SignedDuration::from_secs(3600)).unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap();

    assert_eq!(
        longest_verdict(
            vec![
                rl_window_mins(90, Some(passed), 300),
                rl_window_mins(50, Some(mid), 7 * 24 * 60),
            ],
            now,
        ),
        Some(true),
        "a running longest window skips even when the short window expired"
    );
    assert_eq!(
        longest_verdict(
            vec![
                rl_window_mins(50, Some(five_hour_full), 300),
                rl_window_mins(1, Some(seven_day_full), 7 * 24 * 60),
            ],
            now,
        ),
        Some(false),
        "a not-started longest window primes even when the short window runs"
    );

    let unknown = RateLimitWindow {
        used_percentage: None,
        resets_at: Some(mid),
        duration_mins: Some(7 * 24 * 60),
        ..Default::default()
    };
    assert_eq!(
        longest_verdict(vec![unknown], now),
        None,
        "an unknown longest bar yields no verdict"
    );
}

#[test]
fn longest_window_reset_at_reads_the_raw_longest_stamp() {
    let passed = Timestamp::from_second(1_000_000_000).unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let (_dir, runtime) = runtime_with_windows(vec![
        rl_window_mins(40, Some(future), 300),
        rl_window_mins(80, Some(passed), 7 * 24 * 60),
    ]);

    assert_eq!(
        longest_window_reset_at(&runtime, "claude"),
        Some(passed),
        "the reset occurrence uses the raw cache stamp, not projection"
    );

    let (_dir, runtime) = runtime_with_windows(vec![
        rl_window_mins(40, Some(future), 300),
        rl_window_mins(80, None, 7 * 24 * 60),
    ]);
    assert_eq!(
        longest_window_reset_at(&runtime, "claude"),
        None,
        "an undated longest window has no reset occurrence"
    );

    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    assert_eq!(
        longest_window_reset_at(&runtime, "claude"),
        None,
        "a cold cache has no reset occurrence"
    );
}
