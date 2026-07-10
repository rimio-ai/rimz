use super::*;
use crate::DailyBudgetView;

// ── Rate-limit window fusion: the live half (the dashboard bars) ─────────────

/// One session's reading — a set of windows that age out together.
fn reading(windows: impl IntoIterator<Item = RateLimitWindow>) -> AgentRateLimits {
    AgentRateLimits {
        windows: windows.into_iter().collect(),
    }
}

/// A window of `mins` length, `used`% drained, resetting `resets_in_secs` after
/// the [`epoch`] (negative = the reset already passed).
fn window_mins(used: u8, resets_in_secs: i64, mins: u32) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(epoch() + jiff::SignedDuration::from_secs(resets_in_secs)),
        duration_mins: Some(mins),
        ..Default::default()
    }
}

#[test]
fn fresh_windows_keep_most_drained_per_duration() {
    // Two sessions each report a 5h and a 7d window at different drains. Within
    // a live window usage only climbs, so the most-drained reading is the
    // truest and the pick is stable against session order.
    let a = reading([window_mins(20, 3_600, 300), window_mins(40, 3_600, 10_080)]);
    let b = reading([window_mins(50, 1_800, 300), window_mins(10, 3_600, 10_080)]);

    let stable = fresh_windows([&a, &b].into_iter(), epoch());
    assert_eq!(stable.len(), 2, "one bar per duration");
    assert_eq!(
        stable[0].duration_mins,
        Some(300),
        "short window sorts first"
    );
    assert_eq!(stable[0].used_percentage, Some(50), "most-drained 5h kept");
    assert_eq!(
        stable[1].duration_mins,
        Some(10_080),
        "long window sorts last"
    );
    assert_eq!(stable[1].used_percentage, Some(40), "most-drained 7d kept");

    let reversed = fresh_windows([&b, &a].into_iter(), epoch());
    assert_eq!(
        reversed, stable,
        "the pick never flickers with session order"
    );
}

#[test]
fn fresh_windows_reject_a_reading_whose_shortest_window_reset() {
    // An idle session re-emits a stale payload: its 5h window reset long ago,
    // but its 7d reset is still future. The whole reading is dropped, so its
    // stale-high 7d can't outweigh the live low 7d — the per-window check alone
    // would have kept the 59%.
    let live = reading([window_mins(15, 3_600, 300), window_mins(3, 86_400, 10_080)]);
    let stale = reading([
        window_mins(57, -3_600, 300),
        window_mins(59, 86_400, 10_080),
    ]);

    let stable = fresh_windows([&live, &stale].into_iter(), epoch());
    let seven_day = stable
        .iter()
        .find(|window| window.duration_mins == Some(10_080))
        .expect("a 7d bar");
    assert_eq!(
        seven_day.used_percentage,
        Some(3),
        "the stale-high 7d is dropped with its reading"
    );

    // A reading with no dated window can't be aged out — it backstops.
    let undated = reading([RateLimitWindow {
        used_percentage: Some(33),
        resets_at: None,
        duration_mins: Some(300),
        ..Default::default()
    }]);
    let only_stale = reading([window_mins(90, -10, 300)]);
    let stable = fresh_windows([&only_stale, &undated].into_iter(), epoch());
    assert_eq!(stable.len(), 1);
    assert_eq!(
        stable[0].used_percentage,
        Some(33),
        "undated backstops the stale"
    );
}

#[test]
fn fresh_windows_drop_a_window_whose_own_reset_passed_mid_shorter_window() {
    // A 7d resets while the 5h is still mid-cycle. A lagging session keeps
    // re-reporting the pre-reset 7d (high used, reset now past); the
    // reading-level `content_stale_at` gate can't catch it because the shortest
    // (5h) window is still future. Without the per-window epoch skip the lagging
    // 62% would win the most-drained pick over the fresh post-reset 2%.
    let active = reading([window_mins(40, 3_600, 300), window_mins(2, 600_000, 10_080)]);
    let lagging = reading([window_mins(35, 3_600, 300), window_mins(62, -4_000, 10_080)]);

    let stable = fresh_windows([&active, &lagging].into_iter(), epoch());
    let seven_day = stable
        .iter()
        .find(|window| window.duration_mins == Some(10_080))
        .expect("a 7d bar");
    assert_eq!(
        seven_day.used_percentage,
        Some(2),
        "the fresh post-reset 7d wins; the lagging pre-reset epoch is dropped"
    );
    let five_hour = stable
        .iter()
        .find(|window| window.duration_mins == Some(300))
        .expect("a 5h bar");
    assert_eq!(
        five_hour.used_percentage,
        Some(40),
        "both 5h windows are in-epoch, so most-drained still applies"
    );
}

#[test]
fn fresh_windows_replay_captured_free_reset() {
    // 27 real Claude readings captured mid free-reset: the 7d budget refilled
    // (used 75% → ~1–3%) with its reset timer unchanged, while one idle session
    // still reported the pre-reset 7d=59%. That session's 5h reset is ~1.6 days
    // past, so the reading-level staleness check drops it whole — the live bar
    // reads 3%, not the 59% the old most-drained-per-window pick clung to.
    let readings: Vec<AgentRateLimits> =
        serde_json::from_str(include_str!("fixtures/claude_free_reset.json"))
            .expect("captured fixture parses");
    let now = "2026-06-13T06:15:00Z"
        .parse()
        .expect("fixed instant just after capture");

    let stable = fresh_windows(readings.iter(), now);
    let seven_day = stable
        .iter()
        .find(|window| window.duration_mins == Some(10_080))
        .expect("a 7d bar");
    assert_eq!(
        seven_day.used_percentage,
        Some(3),
        "the live refill wins, not the clung 59%"
    );
    let five_hour = stable
        .iter()
        .find(|window| window.duration_mins == Some(300))
        .expect("a 5h bar");
    assert_eq!(
        five_hour.used_percentage,
        Some(18),
        "the stale session's 57% 5h is rejected too"
    );
}

// ── The per-call split: the context line's row-level fallback ────────────────

#[test]
fn call_split_projects_only_with_known_input_sides() {
    // The per-call split a rollout's `last_token_usage` feeds onto the
    // lifecycle rail projects onto the row, and its `filled()` — cache reads +
    // cache writes + fresh input, exactly the window numerator the `▣` percent
    // scales — stands in for the severity axis's absolute-token read when no
    // rich blob exists.
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    codex.cache_read_input_tokens = Some(120_000);
    codex.cache_write_input_tokens = Some(10_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    let snapshot = room_with_agent_panes(vec![codex]);

    let projected = row(&snapshot, "sess-1");
    let split = projected
        .call_split()
        .expect("the split projects onto the row");
    assert_eq!(split.cache_read, 120_000);
    assert_eq!(split.cache_write, 10_000);
    assert_eq!(split.fresh_input, 9_200);
    assert_eq!(split.output, 800);
    assert_eq!(split.filled(), 139_200);
    assert_eq!(projected.context_used_tokens(), Some(139_200));

    // Until the input side of a call is known the row keeps the bare total —
    // a pre-first-turn agent never legends a partial composition.
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    codex.total_tokens = Some(5_000);
    codex.cache_read_input_tokens = Some(99);
    let snapshot = room_with_agent_panes(vec![codex]);

    let projected = row(&snapshot, "sess-1");
    assert_eq!(projected.call_split(), None);
    assert_eq!(projected.context_used_tokens(), None);
}

/// The cockpit's live headline and local-day spend ride the published frame
/// across every snapshot wire — `rimz sidebar snapshot` stdout — so the fields
/// must survive a JSON round-trip, and a frame from a pre-overlay producer must
/// read as `None` (version skew degrades to the walked tally, never an error).
#[test]
fn today_spend_live_usd_round_trips_and_defaults_absent() {
    let mut snapshot = room(Vec::new());
    snapshot.today_spend_live_usd = Some(12.34);
    snapshot.today_spend_epoch_secs = Some(123);
    snapshot.fleet_day_spend_usd = Some(10.50);
    snapshot.fleet_day_spend_epoch_secs = Some(100);
    snapshot.fleet_budget = Some(DailyBudgetView {
        cap_usd: 20.0,
        spend_usd: 10.50,
        parked: false,
    });
    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: SidebarSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.today_spend_live_usd, Some(12.34));
    assert_eq!(parsed.today_spend_epoch_secs, Some(123));
    assert_eq!(parsed.fleet_day_spend_usd, Some(10.50));
    assert_eq!(parsed.fleet_day_spend_epoch_secs, Some(100));
    assert_eq!(parsed.fleet_budget, snapshot.fleet_budget);

    // An old producer's frame carries no field at all (`skip_serializing_if`
    // keeps `None` off the wire symmetrically).
    snapshot.today_spend_live_usd = None;
    snapshot.today_spend_epoch_secs = None;
    snapshot.fleet_day_spend_usd = None;
    snapshot.fleet_day_spend_epoch_secs = None;
    snapshot.fleet_budget = None;
    let bare = serde_json::to_string(&snapshot).unwrap();
    assert!(!bare.contains("today_spend_live_usd"));
    assert!(!bare.contains("today_spend_epoch_secs"));
    assert!(!bare.contains("fleet_day_spend_usd"));
    assert!(!bare.contains("fleet_day_spend_epoch_secs"));
    assert!(!bare.contains("fleet_budget"));
    let parsed: SidebarSnapshot = serde_json::from_str(&bare).unwrap();
    assert_eq!(parsed.today_spend_live_usd, None);
    assert_eq!(parsed.today_spend_epoch_secs, None);
    assert_eq!(parsed.fleet_day_spend_usd, None);
    assert_eq!(parsed.fleet_day_spend_epoch_secs, None);
    assert_eq!(parsed.fleet_budget, None);
}
