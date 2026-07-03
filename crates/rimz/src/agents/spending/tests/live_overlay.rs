use super::*;

#[test]
fn live_overlay_cases_stay_bounded() {
    for (name, walked, live, baselines, published_at, expected) in [
        (
            "overshoot",
            10.0,
            vec![("a", 1.30, Some(5_000)), ("b", 2.50, Some(5_000))],
            BTreeMap::from([("a".to_owned(), 1.00), ("b".to_owned(), 2.50)]),
            9_000,
            10.30,
        ),
        (
            "new session",
            1.00,
            vec![("fresh", 0.40, Some(9_500))],
            BTreeMap::new(),
            9_000,
            1.40,
        ),
        (
            "unbaselined old sessions",
            5.00,
            vec![("old", 3.00, Some(8_000)), ("unstamped", 2.00, None)],
            BTreeMap::new(),
            9_000,
            5.00,
        ),
        (
            "negative delta",
            6.00,
            vec![("a", 3.20, Some(5_000))],
            BTreeMap::from([("a".to_owned(), 4.00)]),
            9_000,
            6.00,
        ),
    ] {
        let blended = today_spend_live_usd(walked, live.into_iter(), &baselines, published_at);
        assert!((blended - expected).abs() < 1e-9, "{name}");
    }
}

#[test]
fn workspace_carry_reconciles_monotone_displays_and_reset_edges() {
    let prev = workspace_cache_for_carry(
        "scope",
        100.0,
        123,
        0.0,
        10_000,
        BTreeMap::from([("a".to_owned(), 5.0)]),
    );
    let live_costs = vec![("a".to_owned(), 7.0, Some(1_000))];
    let (carry, baselines) = reconcile_workspace_carry(&prev, "scope", 101.0, 123, &live_costs);
    assert!((carry - 1.0).abs() < 1e-9);
    assert_eq!(baselines, BTreeMap::from([("a".to_owned(), 7.0)]));

    let prev = workspace_cache_for_carry("scope", 101.0, 123, carry, 20_000, baselines);
    let (carry, _) = reconcile_workspace_carry(&prev, "scope", 101.75, 123, &live_costs);
    assert!((carry - 0.25).abs() < 1e-9);

    let reset_prev = workspace_cache_for_carry(
        "scope",
        100.0,
        123,
        2.0,
        10_000,
        BTreeMap::from([("a".to_owned(), 5.0)]),
    );
    let live_reset = vec![("a".to_owned(), 8.0, Some(1_000))];
    let (epoch_carry, epoch_baselines) =
        reconcile_workspace_carry(&reset_prev, "scope", 100.0, 456, &live_reset);
    assert_eq!(epoch_carry, 0.0);
    assert_eq!(epoch_baselines, BTreeMap::from([("a".to_owned(), 8.0)]));
    assert_eq!(
        reconcile_workspace_carry(&reset_prev, "other", 100.0, 123, &live_reset).0,
        0.0
    );
    let old_version = WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION - 1,
        ..reset_prev
    };
    assert_eq!(
        reconcile_workspace_carry(&old_version, "scope", 100.0, 123, &live_reset).0,
        0.0
    );

    let scope = "scope";
    let cutoff = 123;
    let mut cache = workspace_cache_for_carry(
        scope,
        100.0,
        cutoff,
        0.0,
        10_000,
        BTreeMap::from([("a".to_owned(), 10.0)]),
    );
    let mut displays = Vec::new();

    let live_a = vec![("a".to_owned(), 12.0, Some(1_000))];
    displays.push(displayed_workspace_usd(&cache, &live_a));
    cache = publish_workspace_for_carry(&cache, scope, 101.0, cutoff, 20_000, &live_a);
    displays.push(displayed_workspace_usd(&cache, &live_a));

    let live_b = vec![("a".to_owned(), 13.0, Some(1_000))];
    displays.push(displayed_workspace_usd(&cache, &live_b));
    cache = publish_workspace_for_carry(&cache, scope, 102.0, cutoff, 30_000, &live_b);
    displays.push(displayed_workspace_usd(&cache, &live_b));

    cache = publish_workspace_for_carry(&cache, scope, 103.0, cutoff, 40_000, &live_b);
    displays.push(displayed_workspace_usd(&cache, &live_b));

    assert_eq!(displays, vec![102.0, 102.0, 103.0, 103.0, 103.0]);
}
