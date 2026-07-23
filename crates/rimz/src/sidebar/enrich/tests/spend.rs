//! Spending caches read back by a consumer fold: the global provider tally,
//! the per-workspace scoped tally, and the version and publication gates.

use super::*;

#[test]
fn cached_enrich_reads_workspace_spending_cache_separately_from_global() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let project = dir.path().join("repo");
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now())
        .with_project_root(Some(project.clone()));

    let mut global = crate::agents::spending::Spending::default();
    global.total.headline.usd = 50.0;
    global.total.year.usd = 50.0;
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        unix_now_ms(),
        &global,
    );

    let mut scoped = crate::SpendTally::default();
    scoped.headline.usd = 1.25;
    scoped.headline.sessions = 3;
    scoped.year.usd = 1.25;
    let hash = workspace_scope_hash(&project);
    // An ancient stamp (`refreshed_at_ms = 1`): age is ignored once the hash
    // matches, so consumer tabs hold the last matching workspace tally instead
    // of flapping to zero.
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(&hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: 1,
            scope_hash: hash.clone(),
            tally: scoped,
            ..Default::default()
        },
    );

    snapshot = fold_cached(snapshot, None, &runtime);

    assert_eq!(
        snapshot
            .value_tally
            .as_ref()
            .map(|tally| tally.headline.usd),
        Some(50.0),
        "global tally remains available for the store"
    );
    assert_eq!(
        snapshot
            .workspace_value_tally
            .as_ref()
            .map(|tally| (tally.headline.usd, tally.headline.sessions)),
        Some((1.25, 3)),
        "cockpit tally comes from the hash-matching workspace cache regardless of age"
    );
}

#[test]
fn cached_enrich_ignores_old_provider_spending_version() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());

    let mut global = crate::agents::spending::Spending::default();
    global.total.month.usd = 99.0;
    global.total.year.usd = 99.0;
    let old = crate::agents::spending::ProviderSpendingCache {
        version: crate::agents::spending::PROVIDER_SPENDING_VERSION - 1,
        refreshed_at_ms: unix_now_ms(),
        spending: global,
        ..Default::default()
    };
    atomic::write_temp_then_rename_cache(&runtime.shared_provider_spending_path(), &old).unwrap();

    let snapshot = fold_cached(snapshot, None, &runtime);

    assert_eq!(
        snapshot.value_tally, None,
        "consumer folds ignore stale aggregate shapes instead of displaying old sidebar history"
    );
}

#[test]
fn cached_enrich_waits_for_producer_workspace_publication() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            providers: BTreeMap::new(),
        },
    )
    .unwrap();

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
            stat: crate::agents::TranscriptStat {
                mtime_secs: 1,
                len: 1,
                ..crate::agents::TranscriptStat::default()
            },
            cursor: crate::agents::spending::SpendCursor::default(),
            origin_path: Some(project.join("src")),
            entries: vec![crate::agents::spending::CachedEntry {
                ts_secs: now_secs,
                cost_usd: 3.75,
                input: 20,
                output: 7,
                cache_write: 0,
                cache_read: 0,
                tool_calls: Default::default(),
                message_id: Some("msg-miss".to_owned()),
                request_id: Some("req-miss".to_owned()),
                dedup_key: None,
                thread_id: None,
                is_sidechain: false,
                has_speed: false,
                model: None,
                rolled: false,
            }],
            unknown_models: BTreeMap::new(),
        },
    );
    crate::agents::spending::write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let mut provider_spending = crate::agents::spending::Spending::default();
    provider_spending.total.headline.usd = 9.0;
    provider_spending.total.year.usd = 9.0;
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        published_ms,
        &provider_spending,
    );

    let hash = workspace_scope_hash(&project);
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "test starts without the per-scope workspace cache"
    );

    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now())
        .with_project_root(Some(project));
    let project = snapshot.project_root.clone().unwrap();
    let mut agent = root_agent("claude", "new-session", None);
    agent.worktree_path = Some(project.display().to_string());
    agent.transcript_path = Some(transcript.display().to_string());
    snapshot.agents = vec![agent];
    snapshot.worktree_groups = vec![worktree_group(
        &project,
        vec![cost_row_at(
            "new-session",
            Some(0.25),
            Some(registered_after_publish),
            &project,
        )],
    )];
    let mut config = crate::config::MachineConfig::default();
    config.sidebar.spend_window = crate::agents::spending::SpendWindowMode::Today;
    let missing = fold(
        snapshot.clone(),
        None,
        &runtime,
        FoldOpts {
            config: Some(std::sync::Arc::new(config.clone())),
            ..cached_opts()
        },
    );

    assert_eq!(missing.value_tally.unwrap().headline.usd, 9.0);
    assert_eq!(missing.workspace_value_tally, None);
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "consumer never creates a missing workspace sidecar"
    );

    let mut tally = crate::agents::spending::SpendTally::default();
    tally.headline = crate::agents::spending::SpendWindow {
        usd: 3.75,
        tokens: 27,
        sessions: 1,
        ..Default::default()
    };
    tally.year = tally.headline.clone();
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(&hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            version: crate::agents::spending::WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms: published_ms,
            scope_hash: hash,
            tally,
            live_baselines: BTreeMap::from([(transcript.display().to_string(), 3.75)]),
            ..Default::default()
        },
    );
    let published = fold(
        snapshot,
        None,
        &runtime,
        FoldOpts {
            config: Some(std::sync::Arc::new(config)),
            ..cached_opts()
        },
    );
    assert_eq!(published.workspace_value_tally.unwrap().headline.usd, 3.75);
}
