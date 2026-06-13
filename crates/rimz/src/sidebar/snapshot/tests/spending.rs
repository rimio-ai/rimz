use super::*;

/// A cost-bearing agent row for the overlay glue: `id`, the statusline
/// `total_cost_usd`, and the registration stamp are the three fields
/// [`live_row_costs`] projects.
fn cost_row(id: &str, usd: Option<f64>, registered_at: Option<Timestamp>) -> crate::SidebarRow {
    cost_row_at(id, usd, registered_at, Path::new("/repo/wt"))
}

/// The cockpit scope hash for a project root, derived the way cached enrich
/// derives it: project root plus the durable worktree home resolved from the
/// loaded machine config. Tests that pre-write a per-scope workspace cache key
/// it through here so the consumer reads back the same hash.
fn workspace_scope_hash(project: &Path) -> String {
    let config = crate::config::MachineConfig::load().unwrap_or_default();
    let home = crate::worktree::worktree_parent(project, &config.worktree).ok();
    crate::agents::spending::SpendScope::for_workspace(Some(project), &[], home.as_deref()).hash()
}

fn cost_row_at(
    id: &str,
    usd: Option<f64>,
    registered_at: Option<Timestamp>,
    worktree_path: &Path,
) -> crate::SidebarRow {
    let mut row = activity_row(
        true,
        Some(AgentStatus::Running),
        Timestamp::from_second(1_750_000_000).unwrap(),
        worktree_path,
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
    let external = Path::new("/tmp/rimz-other-project");
    let build_snapshot = || {
        let mut snapshot =
            SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new(), published)
                .with_project_root(Some(dir.path().to_path_buf()));
        snapshot.worktree_groups = vec![worktree_group(
            &wt,
            vec![
                cost_row_at("baselined", Some(2.00), Some(before), &wt),
                cost_row_at("external", Some(9.00), Some(before), external),
            ],
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

    let compute_spending = |_: &SidebarSnapshot| crate::agents::spending::SpendingCaches {
        workspace: crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: walk_ms,
            ..Default::default()
        },
        ..Default::default()
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
        !advanced.baselines.contains_key("external"),
        "producer baselines are captured from the workspace-scoped live rows"
    );
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
    let linked = Path::new("/linked/wt");

    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(wt),
        Vec::new(),
        Vec::new(),
        published,
    )
    .with_project_root(Some(Path::new("/repo").to_path_buf()))
    .with_worktree_roots(vec![linked.to_path_buf()]);
    snapshot.worktree_groups = vec![
        worktree_group(
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
        ),
        worktree_group(
            linked,
            vec![cost_row_at(
                "linked-newborn",
                Some(0.20),
                Some(after),
                linked,
            )],
        ),
    ];

    let baselines = BTreeMap::from([("baselined".to_owned(), 5.00)]);

    apply_live_today_spend(&mut snapshot, 10.0, published_ms, &baselines);
    let live = snapshot.today_spend_live_usd.expect("a spent day stamps");
    assert!(
        (live - 11.00).abs() < 1e-9,
        "walked 10.00 + 0.50 + 0.30 + 0.20"
    );

    // The zero gate: an empty room on an unspent day keeps the field bare so
    // the cockpit holds its bare `¤` line.
    let mut empty = SidebarSnapshot::build(
        WorkspaceId::from_project_root(wt),
        Vec::new(),
        Vec::new(),
        published,
    )
    .with_project_root(Some(Path::new("/repo").to_path_buf()));
    apply_live_today_spend(&mut empty, 0.0, 0, &BTreeMap::new());
    assert_eq!(empty.today_spend_live_usd, None);
}

#[test]
fn apply_live_today_spend_excludes_out_of_scope_live_rows() {
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let published_ms = published.as_millisecond() as u64;
    let before = published - SignedDuration::from_secs(600);
    let after = published + SignedDuration::from_secs(5);
    let project = Path::new("/repo/main");
    let other = Path::new("/tmp/other");

    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(project),
        Vec::new(),
        Vec::new(),
        published,
    )
    .with_project_root(Some(project.to_path_buf()));
    snapshot.worktree_groups = vec![worktree_group(
        other,
        vec![
            cost_row_at("external-new", Some(2.00), Some(after), other),
            cost_row_at("external-baselined", Some(5.50), Some(before), other),
        ],
    )];

    let baselines = BTreeMap::from([("external-baselined".to_owned(), 5.00)]);

    apply_live_today_spend(&mut snapshot, 0.0, published_ms, &baselines);
    assert_eq!(
        snapshot.today_spend_live_usd, None,
        "out-of-scope live rows do not add newborn cost or baseline deltas"
    );
}

#[test]
fn cached_enrich_reads_workspace_spending_cache_separately_from_global() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let project = dir.path().join("repo");
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_project_root(Some(project.clone()));

    let mut global = crate::agents::spending::Spending::default();
    global.total.today.usd = 50.0;
    global.total.year.usd = 50.0;
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        unix_now_ms(),
        &global,
    );

    let mut scoped = crate::SpendTally::default();
    scoped.today.usd = 1.25;
    scoped.today.sessions = 3;
    scoped.year.usd = 1.25;
    let hash = workspace_scope_hash(&project);
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(&hash),
        unix_now_ms(),
        &hash,
        &scoped,
    );

    snapshot = enrich(snapshot, None, &runtime, None, EnrichMode::Cached, None);

    assert_eq!(
        snapshot.value_tally.as_ref().map(|tally| tally.today.usd),
        Some(50.0),
        "global tally remains available for the ledger"
    );
    assert_eq!(
        snapshot
            .workspace_value_tally
            .as_ref()
            .map(|tally| (tally.today.usd, tally.today.sessions)),
        Some((1.25, 3)),
        "cockpit tally comes from the workspace cache"
    );
}

#[test]
fn cached_enrich_derives_workspace_spending_from_shared_cursor_on_cache_miss() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let project = dir.path().join("repo");
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = crate::agents::spending::unix_secs_now();
    let published = Timestamp::now() - SignedDuration::from_secs(30);
    let published_ms = published.as_millisecond().max(0) as u64;
    let registered_after_publish = Timestamp::now() - SignedDuration::from_secs(10);
    let mut raw =
        crate::agents::spending::read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        crate::agents::spending::FileCacheEntry {
            mtime_secs: 1,
            len: 1,
            cursor: crate::agents::spending::SpendCursor::default(),
            origin_path: None,
            entries: vec![crate::agents::spending::CachedEntry {
                ts_secs: now_secs,
                cost_usd: 3.75,
                input: 20,
                output: 7,
                cache_write: 0,
                cache_read: 0,
                message_id: Some("msg-miss".to_owned()),
                request_id: Some("req-miss".to_owned()),
                is_sidechain: false,
                origin_path: Some(project.join("src")),
            }],
            unknown_models: BTreeMap::new(),
        },
    );
    crate::agents::spending::write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        published_ms,
        &crate::agents::spending::Spending::default(),
    );

    let hash = workspace_scope_hash(&project);
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "test starts without the per-scope workspace cache"
    );

    let _discovered = crate::agents::spending::override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )]);
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_project_root(Some(project));
    let project = snapshot.project_root.clone().unwrap();
    snapshot.worktree_groups = vec![worktree_group(
        &project,
        vec![cost_row_at(
            "new-session",
            Some(0.25),
            Some(registered_after_publish),
            &project,
        )],
    )];
    let snapshot = enrich(snapshot, None, &runtime, None, EnrichMode::Cached, None);

    assert_eq!(
        snapshot.workspace_value_tally.as_ref().map(|tally| (
            tally.today.usd,
            tally.today.sessions,
            tally.today.tokens
        )),
        Some((3.75, 1, 27)),
        "consumer folds derive the cockpit tally from the shared cursor cache"
    );
    assert_eq!(
        snapshot.today_spend_live_usd,
        Some(4.0),
        "the derived cache keeps the producer publish stamp for live-spend overlay"
    );
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "the consumer derive path stays read-only"
    );
}

#[test]
fn cached_enrich_uses_hash_matching_workspace_cache_regardless_of_age() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let project = dir.path().join("repo");
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_project_root(Some(project.clone()));
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        unix_now_ms(),
        &crate::agents::spending::Spending::default(),
    );

    let mut scoped = crate::SpendTally::default();
    scoped.today.usd = 2.50;
    scoped.today.sessions = 4;
    scoped.year.usd = 2.50;
    let hash = workspace_scope_hash(&project);
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(&hash),
        1,
        &hash,
        &scoped,
    );

    snapshot = enrich(snapshot, None, &runtime, None, EnrichMode::Cached, None);

    assert_eq!(
        snapshot
            .workspace_value_tally
            .as_ref()
            .map(|tally| (tally.today.usd, tally.today.sessions)),
        Some((2.50, 4)),
        "consumer tabs hold the last matching workspace tally instead of flapping to zero"
    );
}
