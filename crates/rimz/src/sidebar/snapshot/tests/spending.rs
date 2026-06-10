use super::*;

/// A cost-bearing agent row for the overlay glue: `id`, the statusline
/// `total_cost_usd`, and the registration stamp are the three fields
/// [`live_row_costs`] projects.
fn cost_row(id: &str, usd: Option<f64>, registered_at: Option<Timestamp>) -> crate::SidebarRow {
    let mut row = activity_row(
        true,
        Some(AgentStatus::Running),
        Timestamp::from_second(1_750_000_000).unwrap(),
        Path::new("/repo/wt"),
    );
    row.id = id.to_owned();
    let agent = row.as_agent_mut().unwrap();
    agent.registered_at = registered_at;
    agent.context = usd.map(|usd| crate::agents::AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: Some(crate::agents::AgentCost {
            total_cost_usd: Some(usd),
            ..Default::default()
        }),
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_error: None,
        observed_at: Timestamp::from_second(1_750_000_000).unwrap(),
    });
    row
}

#[test]
fn live_spend_baselines_are_written_only_by_producer_enrich() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            refreshed_at_ms: unix_now_ms(),
            accounts: BTreeMap::new(),
            ok: true,
        },
    )
    .unwrap();

    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let walk_ms = published.as_millisecond() as u64;
    let before = published - SignedDuration::from_secs(60);
    let wt = dir.path().join("wt");
    let build_snapshot = || {
        let mut snapshot =
            SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new(), published);
        snapshot.worktree_groups = vec![worktree_group(
            &wt,
            vec![cost_row("baselined", Some(2.00), Some(before))],
        )];
        snapshot
    };

    let spending = crate::agents::spending::Spending::default();
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        walk_ms,
        &spending,
    );
    let baseline_path = runtime.live_spend_baselines_path();

    let _ = enrich(
        build_snapshot(),
        None,
        &runtime,
        None,
        EnrichMode::Cached,
        None,
    );
    assert!(
        !baseline_path.exists(),
        "consumer folds read baselines but never create the sidecar"
    );

    let stale = crate::agents::spending::LiveSpendBaselines {
        observed_walk_ms: 10,
        baselines: BTreeMap::from([("old".to_owned(), 0.50)]),
    };
    crate::agents::spending::write_live_spend_baselines(&baseline_path, &stale);
    let _ = enrich(
        build_snapshot(),
        None,
        &runtime,
        None,
        EnrichMode::Cached,
        None,
    );
    assert_eq!(
        crate::agents::spending::read_live_spend_baselines(&baseline_path),
        stale,
        "consumer folds do not advance an existing baseline sidecar"
    );

    let compute_spending = |_: &SidebarSnapshot| ProviderSpendingCache {
        refreshed_at_ms: walk_ms,
        ..ProviderSpendingCache::default()
    };
    let refresh_git = |_: &mut SidebarSnapshot| {};
    let _ = enrich(
        build_snapshot(),
        None,
        &runtime,
        None,
        EnrichMode::Producing {
            roots: None,
            compute_spending: &compute_spending,
            config: Box::new(crate::config::MachineConfig::default()),
            refresh_git: &refresh_git,
        },
        None,
    );
    let advanced = crate::agents::spending::read_live_spend_baselines(&baseline_path);
    assert_eq!(advanced.observed_walk_ms, walk_ms);
    assert_eq!(advanced.baselines.get("baselined"), Some(&2.00));
    assert!(
        !advanced.baselines.contains_key("old"),
        "a producer walk replaces the prior baseline set for the new stamp"
    );
}

/// The consumer overlay glue end-to-end over a built snapshot:
/// [`live_row_costs`] projects each agent row's `(id, statusline cost,
/// registered-at)` triple and [`apply_live_today_spend`] stamps the walked
/// floor plus per-session overshoot — exercising the row-id ↔ baseline join
/// the producer and every consumer tab rely on, the new-session rule against
/// the cache's publish stamp, and the zero gate.
#[test]
fn apply_live_today_spend_stamps_overshoot_over_the_walked_floor() {
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let published_ms = published.as_millisecond() as u64;
    let before = published - SignedDuration::from_secs(600);
    let after = published + SignedDuration::from_secs(5);
    let wt = Path::new("/repo/wt");

    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(wt),
        Vec::new(),
        Vec::new(),
        published,
    );
    snapshot.worktree_groups = vec![worktree_group(
        wt,
        vec![
            // Baselined at $5.00, now $5.50: contributes the $0.50 overshoot.
            cost_row("baselined", Some(5.50), Some(before)),
            // Born after the publish: the walk never saw it, whole cost counts.
            cost_row("newborn", Some(0.30), Some(after)),
            // Pre-publish but unbaselined (a race): fails safe to zero.
            cost_row("unbaselined", Some(2.00), Some(before)),
            // No statusline cost yet: skipped by the projection.
            cost_row("costless", None, Some(before)),
        ],
    )];

    let mut cache = ProviderSpendingCache {
        refreshed_at_ms: published_ms,
        ..ProviderSpendingCache::default()
    };
    cache.spending.total.today.usd = 10.0;
    let baselines = BTreeMap::from([("baselined".to_owned(), 5.00)]);

    apply_live_today_spend(&mut snapshot, &cache, &baselines);
    let live = snapshot.today_spend_live_usd.expect("a spent day stamps");
    assert!((live - 10.80).abs() < 1e-9, "walked 10.00 + 0.50 + 0.30");

    // The zero gate: an empty room on an unspent day keeps the field bare so
    // the cockpit holds its bare `¤` line.
    let mut empty = SidebarSnapshot::build(
        WorkspaceId::from_project_root(wt),
        Vec::new(),
        Vec::new(),
        published,
    );
    apply_live_today_spend(
        &mut empty,
        &ProviderSpendingCache::default(),
        &BTreeMap::new(),
    );
    assert_eq!(empty.today_spend_live_usd, None);
}
