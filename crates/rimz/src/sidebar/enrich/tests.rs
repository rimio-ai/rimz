use super::*;
use crate::agents::TurnPhase;
use crate::feed::AgentStatus;
use crate::ledger::atomic;
use crate::remote::link::{LinkStats, LinkStatsFile, LinkTier};
use crate::sidebar::cache::{AccountsCache, unix_now_ms};
use crate::sidebar::test_support::{activity_row, pane, worktree_group};
use jiff::SignedDuration;
use std::collections::BTreeMap;
use std::path::Path;

fn runtime() -> (tempfile::TempDir, RuntimePaths, SidebarSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    (dir, runtime, snapshot)
}

fn stats(rtt_ms: Option<u32>, miss_pct: u16) -> LinkStats {
    LinkStats {
        rtt_ms,
        miss_pct,
        window: 30,
        bandwidth_bps: None,
    }
}

#[test]
fn link_stats_sidecar_folds_into_snapshot() {
    let (_dir, runtime, mut snapshot) = runtime();
    let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(230), 4));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    fold_link_stats(&mut snapshot, &runtime, 1_500);

    let link = snapshot.link.expect("link badge");
    assert_eq!(link.rtt_ms, Some(230));
    assert_eq!(link.miss_pct, 4);
    assert_eq!(link.tier, LinkTier::Degraded);
    assert_eq!(link.freshness, crate::SidebarLinkFreshness::Fresh);
    assert_eq!(link.sampled_at_ms, 1_000);
}

#[test]
fn stale_link_stats_render_as_stale_until_expired() {
    let (_dir, runtime, mut snapshot) = runtime();
    let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    fold_link_stats(&mut snapshot, &runtime, 12_000);
    assert_eq!(
        snapshot.link.as_ref().unwrap().freshness,
        crate::SidebarLinkFreshness::Stale
    );

    fold_link_stats(&mut snapshot, &runtime, 122_001);
    assert!(snapshot.link.is_none(), "expired stats disappear");
}

#[test]
fn corrupt_or_wrong_version_stats_disappear() {
    let (_dir, runtime, mut snapshot) = runtime();
    atomic::write_bytes_atomically(&crate::remote::link::stats_path(&runtime), b"not json")
        .unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none());

    let path = crate::remote::link::stats_path(&runtime);
    let mut file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    file.v = "rimz.link.v0".to_owned();
    atomic::write_temp_then_rename_cache(&path, &file).unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none());
}

/// A minimal agent/process row for the hot-set tests: only the fields
/// `hot_worktree_paths` reads vary.
/// The hot set, boundary-exact at `GIT_ACTIVITY_WINDOW`: running rows are hot
/// regardless of activity age, recent activity is hot through exactly the
/// window, process-only and `External`-kind groups are cold, and a dead dir
/// is excluded just as `needed_worktree_paths` excludes it.
#[test]
fn hot_worktree_paths_keys_on_running_or_recent_agent_rows() {
    let dir = tempfile::tempdir().unwrap();
    let wt = |name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let window = SignedDuration::try_from(GIT_ACTIVITY_WINDOW).unwrap();
    let stale_activity = now - window - SignedDuration::from_secs(1);

    let running = wt("running");
    let recent = wt("recent");
    let boundary = wt("boundary");
    let idle = wt("idle");
    let procs = wt("procs");
    let external_kind = wt("external-kind");
    let dead = dir.path().join("dead-dir");

    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Vec::new(),
        now,
    );
    snapshot.worktree_groups = vec![
        // Running carries hotness on its own — its activity stamp is stale.
        worktree_group(
            &running,
            vec![activity_row(
                true,
                Some(AgentStatus::Running),
                stale_activity,
                &running,
            )],
        ),
        worktree_group(
            &recent,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                now - SignedDuration::from_secs(1),
                &recent,
            )],
        ),
        // Exactly the window boundary stays hot (<=, matching the TTL gates).
        worktree_group(
            &boundary,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                now - window,
                &boundary,
            )],
        ),
        worktree_group(
            &idle,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                stale_activity,
                &idle,
            )],
        ),
        // A busy process row is not an agent row: cold by definition.
        worktree_group(&procs, vec![activity_row(false, None, now, &procs)]),
        // An External-kind group never reaches the git refresh.
        {
            let mut group = worktree_group(
                &external_kind,
                vec![activity_row(
                    true,
                    Some(AgentStatus::Running),
                    now,
                    &external_kind,
                )],
            );
            group.kind = crate::SidebarWorktreeKind::External;
            group
        },
        // A running agent in a since-removed dir: hot <= needed, so excluded.
        worktree_group(
            &dead,
            vec![activity_row(true, Some(AgentStatus::Running), now, &dead)],
        ),
    ];

    let hot = hot_worktree_paths(&snapshot);

    let path_of = |p: &Path| p.display().to_string();
    assert!(hot.contains(&path_of(&running)));
    assert!(hot.contains(&path_of(&recent)));
    assert!(
        hot.contains(&path_of(&boundary)),
        "boundary-exact: <= window"
    );
    assert!(!hot.contains(&path_of(&idle)), "stale activity is cold");
    assert!(
        !hot.contains(&path_of(&procs)),
        "process rows carry no heat"
    );
    assert!(!hot.contains(&path_of(&external_kind)));
    assert!(!hot.contains(&path_of(&dead)), "hot is a subset of needed");
    assert_eq!(hot.len(), 3);
}

/// A future-stamped row (clock skew between writers) reads as hot — the safe
/// direction, mirroring the saturating TTL convention.
#[test]
fn hot_worktree_paths_treats_future_activity_as_hot() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Vec::new(),
        now,
    );
    snapshot.worktree_groups = vec![worktree_group(
        dir.path(),
        vec![activity_row(
            true,
            Some(AgentStatus::Idle),
            now + SignedDuration::from_secs(120),
            dir.path(),
        )],
    )];

    assert!(hot_worktree_paths(&snapshot).contains(&dir.path().display().to_string()));
}

#[test]
fn root_pod_is_excluded_from_git_reads() {
    // The root pod of a non-repo room is a known non-repo: it never enters
    // the producer's git fan-out, while child-repo pods do.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let child = dir.path().join("query-engine");
    std::fs::create_dir_all(&child).unwrap();
    let root_cwd = dir.path().to_string_lossy().into_owned();
    let child_cwd = child.to_string_lossy().into_owned();

    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_root_class(crate::workspace::RootClass::Directory)
        .with_project_root(Some(dir.path().to_path_buf()))
        .with_worktree_roots(vec![child.clone()])
        .with_live_panes(
            vec![
                pane("terminal_0", "zsh", &root_cwd),
                pane("terminal_1", "claude", &child_cwd),
            ],
            None,
        );

    let kinds: Vec<SidebarWorktreeKind> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![SidebarWorktreeKind::Root, SidebarWorktreeKind::Worktree]
    );
    assert_eq!(needed_worktree_paths(&snapshot), vec![child_cwd]);
}

/// The config fold stamps every *agent* row's context-severity verdict from
/// the `[theme.display.context_meter]` bands — the one classification the renderer's color
/// ramp and any future signal emitter read — and leaves process rows `None`.
#[test]
fn config_fold_stamps_agent_context_severity() {
    let agent_row = |pct: Option<u8>| crate::SidebarRow {
        id: "row".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        unread: false,
        inactive: false,
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status: Some(AgentStatus::Running),
            phase: TurnPhase::Idle,
            context_pct: pct,
            ..crate::AgentCard::default()
        })),
    };
    let process_row = || crate::SidebarRow {
        id: "row".to_owned(),
        name: "zsh".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        unread: false,
        inactive: false,
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    };
    let mut groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![agent_row(Some(85)), agent_row(Some(5)), process_row()],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
        landed: None,
    }];

    stamp_context_severity(&mut groups, &crate::config::ContextMeterConfig::default());

    let rows = &groups[0].rows;
    assert_eq!(
        rows[0].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::feed::ContextSeverity::Amber),
        "85% crosses the default amber band"
    );
    assert_eq!(
        rows[1].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::feed::ContextSeverity::Calm)
    );
    assert_eq!(
        rows[2].as_agent().and_then(|agent| agent.context_severity),
        None,
        "a process row carries no context verdict"
    );
}

/// The cockpit scope hash for a project root, derived the way cached enrich
/// derives it: project root plus the durable worktree home resolved from the
/// loaded machine config. Tests that pre-write a per-scope workspace cache key
/// it through here so the consumer reads back the same hash.
fn workspace_scope_hash(project: &Path) -> String {
    let config = crate::config::MachineConfig::load().unwrap_or_default();
    let home = crate::worktree::worktree_parent(project, &config.agents.worktree).ok();
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
        turn_complete: None,
        observed_at: Timestamp::from_second(1_750_000_000).unwrap(),
    });
    row
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
        1,
        &hash,
        &scoped,
    );

    snapshot = enrich(snapshot, None, &runtime, None, EnrichMode::Cached, None);

    assert_eq!(
        snapshot
            .value_tally
            .as_ref()
            .map(|tally| tally.headline.usd),
        Some(50.0),
        "global tally remains available for the ledger"
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
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());

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

    let snapshot = enrich(snapshot, None, &runtime, None, EnrichMode::Cached, None);

    assert_eq!(
        snapshot.value_tally, None,
        "consumer folds ignore stale aggregate shapes instead of displaying old sidebar history"
    );
}

#[test]
fn cached_enrich_derives_workspace_spending_from_shared_cursor_on_cache_miss() {
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
                thread_id: None,
                is_sidechain: false,
                model: None,
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
            tally.headline.usd,
            tally.headline.sessions,
            tally.headline.tokens
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
