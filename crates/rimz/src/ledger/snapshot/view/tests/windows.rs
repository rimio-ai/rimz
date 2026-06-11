use super::*;

// ── Rate-limit window stabilizers (the dashboard bars) ───────────────────────

#[test]
fn stable_windows_keep_conservative_readings_per_duration() {
    // A stale window (reset already passed) reads low; two live windows
    // report 50% and 80%. The stale one is dropped, and the most-drained
    // live survivor (80%) wins — never over-promising remaining budget.
    let live_50 = window(50, 3_600);
    let live_80 = window(80, 1_800);
    let stale_10 = window(10, -60);

    let pick = stable_window(
        [live_50.clone(), live_80.clone(), stale_10.clone()].into_iter(),
        epoch(),
    )
    .expect("a live window survives");
    assert_eq!(pick.used_percentage, Some(80));

    // Order-independent: the producer must not flicker with session order.
    let reversed = stable_window([stale_10, live_80, live_50].into_iter(), epoch())
        .expect("a live window survives");
    assert_eq!(reversed.used_percentage, Some(80));

    assert!(
        stable_window([window(90, -10), window(40, -3_600)].into_iter(), epoch()).is_none(),
        "every dated reading is stale"
    );

    // A window with no reset instant can't be aged out; it is the last-resort
    // reading only when nothing with a live reset survives.
    let undated = RateLimitWindow {
        used_percentage: Some(33),
        resets_at: None,
        duration_mins: Some(300),
    };
    let pick = stable_window([window(90, -10), undated].into_iter(), epoch())
        .expect("the undated reading backstops the stale one");
    assert_eq!(pick.used_percentage, Some(33));

    let mk = |used: u8, mins: u32| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(epoch() + std::time::Duration::from_secs(3_600)),
        duration_mins: Some(mins),
    };
    // Two sessions, each reporting a 5h and a 30d window at different drains.
    let readings = [mk(10, 43_800), mk(20, 300), mk(40, 43_800), mk(5, 300)];
    let stable = stable_windows(readings.into_iter(), epoch());
    assert_eq!(stable.len(), 2, "one bar per duration");
    assert_eq!(
        stable[0].duration_mins,
        Some(300),
        "short window sorts first"
    );
    assert_eq!(stable[0].used_percentage, Some(20), "most-drained 5h kept");
    assert_eq!(
        stable[1].duration_mins,
        Some(43_800),
        "long window sorts last"
    );
    assert_eq!(stable[1].used_percentage, Some(40), "most-drained 30d kept");
}

// ── The per-call split: the context line's row-level fallback ────────────────

#[test]
fn call_split_projects_only_with_known_input_sides() {
    // The per-call split a rollout's `last_token_usage` feeds onto the
    // lifecycle rail projects onto the row, and its `filled()` — cache reads +
    // fresh input, exactly the window numerator the `▣` percent scales —
    // stands in for the severity axis's absolute-token read when no rich blob
    // exists.
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    codex.cache_read_input_tokens = Some(120_000);
    codex.fresh_input_tokens = Some(9_200);
    codex.output_tokens = Some(800);
    let snapshot = room_with_agent_panes(Vec::new(), vec![codex]);

    let projected = row(&snapshot, "sess-1");
    let split = projected
        .call_split()
        .expect("the split projects onto the row");
    assert_eq!(split.cache_read, 120_000);
    assert_eq!(split.fresh_input, 9_200);
    assert_eq!(split.output, 800);
    assert_eq!(split.filled(), 129_200);
    assert_eq!(projected.context_used_tokens(), Some(129_200));

    // Until the input side of a call is known the row keeps the bare total —
    // a pre-first-turn agent never legends a partial composition.
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
    codex.total_tokens = Some(5_000);
    codex.cache_read_input_tokens = Some(99);
    let snapshot = room_with_agent_panes(Vec::new(), vec![codex]);

    let projected = row(&snapshot, "sess-1");
    assert_eq!(projected.call_split(), None);
    assert_eq!(projected.context_used_tokens(), None);
}

/// The cockpit's live today-spend rides the published frame across every
/// snapshot wire — `rimz sidebar snapshot` stdout, the plugin rail — so the
/// field must survive a JSON round-trip, and a frame from a pre-overlay
/// producer must read as `None` (version skew degrades to the walked tally,
/// never an error).
#[test]
fn today_spend_live_usd_round_trips_and_defaults_absent() {
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.today_spend_live_usd = Some(12.34);
    let json = serde_json::to_string(&snapshot).unwrap();
    let parsed: SidebarSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.today_spend_live_usd, Some(12.34));

    // An old producer's frame carries no field at all (`skip_serializing_if`
    // keeps `None` off the wire symmetrically).
    snapshot.today_spend_live_usd = None;
    let bare = serde_json::to_string(&snapshot).unwrap();
    assert!(!bare.contains("today_spend_live_usd"));
    let parsed: SidebarSnapshot = serde_json::from_str(&bare).unwrap();
    assert_eq!(parsed.today_spend_live_usd, None);
}
