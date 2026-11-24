use std::path::Path;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::RuntimePaths;
use crate::agents::account::AccountsCache;
use crate::agents::spending::{
    HeadlineSpec, SilentWalk, SpendWindowMode, SpendingFile, SpendingWalker, WalkRequest,
};
use crate::agents::{AgentState, AgentStatus};
use crate::disk::atomic;
use crate::ids::WorkspaceId;
use crate::sidebar::enrich::{FoldOpts, enrich};
use crate::sidebar::test_support::{activity_row, worktree_group};

fn cached_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: false,
        fresh_roots: None,
        config: None,
        lanes: None,
        agent_projection: Default::default(),
    }
}

fn cost_row(
    id: &str,
    usd: Option<f64>,
    registered_at: Option<Timestamp>,
) -> crate::store::snapshot::SidebarRow {
    cost_row_at(id, usd, registered_at, Path::new("/repo/wt"), None)
}

fn cost_row_at(
    id: &str,
    usd: Option<f64>,
    registered_at: Option<Timestamp>,
    worktree_path: &Path,
    last_activity: Option<Timestamp>,
) -> crate::store::snapshot::SidebarRow {
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let mut row = activity_row(
        true,
        Some(AgentStatus::Running),
        last_activity.unwrap_or(now),
        worktree_path,
    );
    row.id = id.to_owned();
    let agent = row.as_agent_mut().unwrap();
    agent.registered_at = registered_at;
    agent.context = usd.map(|usd| crate::agents::AgentContext {
        cost: Some(crate::agents::AgentCost {
            total_cost_usd: Some(usd),
            ..Default::default()
        }),
        ..crate::agents::AgentContext::new("claude", now)
    });
    row
}

fn agent_state(
    id: &str,
    worktree_path: &Path,
    transcript_path: &Path,
    now: Timestamp,
) -> AgentState {
    AgentState {
        status: AgentStatus::Running,
        worktree_path: Some(worktree_path.display().to_string()),
        transcript_path: Some(transcript_path.display().to_string()),
        ..crate::testkit::agent_state("claude", id, now)
    }
}

fn transcript(id: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/claude/{id}/chat.jsonl"))
}

#[test]
fn consumer_enrich_reads_live_baseline_workspace_cache_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            logins: BTreeMap::new(),
        },
    )
    .unwrap();

    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let walk_ms = published.as_millisecond() as u64;
    let before = published - SignedDuration::from_secs(60);
    let wt = dir.path().join("wt");
    let external = Path::new("/tmp/rimz-other-project");
    let build_snapshot = || -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot::build(workspace.clone(), Vec::new(), published)
            .with_project_root(Some(dir.path().to_path_buf()));
        snapshot.agents = vec![
            agent_state("baselined", &wt, &transcript("baselined"), published),
            agent_state("external", external, &transcript("external"), published),
        ]
        .into();
        snapshot.worktree_groups = vec![worktree_group(
            &wt,
            vec![
                cost_row_at("baselined", Some(2.00), Some(before), &wt, None),
                cost_row_at("external", Some(9.00), Some(before), external, None),
            ],
        )]
        .into();
        snapshot
    };
    let config = crate::config::MachineConfig::load_lenient();
    let worktree_home = crate::worktree::worktree_parent(dir.path(), &config.agents.worktree).ok();
    let scope = SpendScope::for_workspace(Some(dir.path()), &[], worktree_home.as_deref());
    let scope_hash = scope.hash();

    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &crate::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: walk_ms,
            day_cutoff_secs: crate::agents::spending::local_day_start_secs(walk_ms / 1000, None)
                .unwrap(),
            timezone: "UTC".into(),
            chart: crate::agents::spending::ChartSpend {
                today: (walk_ms / 86_400_000) as i64,
                ..Default::default()
            },
            spending: crate::agents::spending::Spending::default(),
            ..Default::default()
        },
    );
    let mut tally = crate::agents::spending::SpendTally::default();
    tally.headline.usd = 10.0;
    let workspace_cache = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: walk_ms,
        scope_hash: scope_hash.clone(),
        tally,
        live_baselines: BTreeMap::from([("claude:baselined".to_owned(), 1.0)]),
        ..Default::default()
    };
    let workspace_path = runtime.workspace_spending_path(&scope_hash);
    crate::agents::spending::write_workspace_spending_cache(&workspace_path, &workspace_cache);
    let before_bytes = std::fs::read(&workspace_path).unwrap();

    let enriched = enrich(
        build_snapshot(),
        None,
        &crate::StatePaths::under(workspace.clone(), dir.path()).unwrap(),
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );
    assert_eq!(std::fs::read(&workspace_path).unwrap(), before_bytes);
    assert_eq!(enriched.today_spend_live_usd, Some(11.0));
}

#[test]
fn live_spend_adds_only_the_unflushed_delta_and_clamps_regressions() {
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let published_ms = published.as_millisecond() as u64;
    let before = published - SignedDuration::from_secs(600);
    let after = published + SignedDuration::from_secs(5);
    let stale = published - SignedDuration::from_secs(SESSION_GAP_SECS as i64 + 1);
    let wt = Path::new("/repo/wt");
    let linked = Path::new("/linked/wt");

    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(wt), Vec::new(), published)
            .with_project_root(Some(Path::new("/repo").to_path_buf()))
            .with_worktree_roots(vec![linked.to_path_buf()], Default::default());
    snapshot.agents = [
        "baselined",
        "newborn",
        "unbaselined",
        "costless",
        "stale",
        "regressed",
        "linked-newborn",
    ]
    .into_iter()
    .map(|id| {
        let path = if id == "linked-newborn" { linked } else { wt };
        agent_state(id, path, &transcript(id), published)
    })
    .collect::<Vec<_>>()
    .into();
    snapshot.worktree_groups = vec![
        worktree_group(
            wt,
            vec![
                cost_row("baselined", Some(5.50), Some(before)),
                cost_row("newborn", Some(0.30), Some(after)),
                cost_row("unbaselined", Some(2.00), Some(before)),
                cost_row("costless", None, Some(before)),
                cost_row_at("stale", Some(9.00), Some(before), wt, Some(stale)),
                cost_row("regressed", Some(4.00), Some(before)),
            ],
        ),
        worktree_group(
            linked,
            vec![cost_row_at(
                "linked-newborn",
                Some(0.20),
                Some(after),
                linked,
                None,
            )],
        ),
    ]
    .into();

    let mut tally = crate::agents::spending::SpendTally::default();
    tally.headline.usd = 10.0;
    let day = crate::agents::spending::SpendWindow {
        usd: 8.0,
        ..Default::default()
    };
    let workspace = WorkspaceSpendingCache {
        refreshed_at_ms: published_ms,
        tally,
        headline_cutoff_secs: 123,
        day,
        day_cutoff_secs: 100,
        live_baselines: BTreeMap::from([
            ("claude:baselined".to_owned(), 5.0),
            ("claude:regressed".to_owned(), 6.0),
        ]),
        ..Default::default()
    };

    apply_live_today_spend(&mut snapshot, &workspace);
    let live = snapshot.today_spend_live_usd.expect("live spend stamps");
    assert!((live - 13.00).abs() < 1e-9);
    assert_eq!(snapshot.today_spend_epoch_secs, Some(123));

    apply_live_day_spend(&mut snapshot, &workspace);
    assert_eq!(snapshot.fleet_day_spend_usd, Some(11.0));
    assert_eq!(snapshot.fleet_day_spend_epoch_secs, Some(100));

    let mut no_burst = workspace.clone();
    no_burst.headline_cutoff_secs = NO_BURST_CUTOFF;
    apply_live_today_spend(&mut snapshot, &no_burst);
    assert_eq!(snapshot.today_spend_live_usd, Some(10.0));
    apply_live_day_spend(&mut snapshot, &no_burst);
    assert_eq!(snapshot.fleet_day_spend_usd, Some(11.0));

    let mut empty =
        SidebarSnapshot::build(WorkspaceId::from_project_root(wt), Vec::new(), published)
            .with_project_root(Some(Path::new("/repo").to_path_buf()));
    apply_live_today_spend(&mut empty, &WorkspaceSpendingCache::default());
    assert_eq!(empty.today_spend_live_usd, Some(0.0));
    apply_live_day_spend(&mut empty, &WorkspaceSpendingCache::default());
    assert_eq!(empty.fleet_day_spend_usd, Some(0.0));
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
        published,
    )
    .with_project_root(Some(project.to_path_buf()));
    snapshot.agents = vec![
        agent_state(
            "external-new",
            other,
            &transcript("external-new"),
            published,
        ),
        agent_state(
            "external-baselined",
            other,
            &transcript("external-baselined"),
            published,
        ),
    ]
    .into();
    snapshot.worktree_groups = vec![worktree_group(
        other,
        vec![
            cost_row_at("external-new", Some(2.00), Some(after), other, None),
            cost_row_at("external-baselined", Some(5.50), Some(before), other, None),
        ],
    )]
    .into();

    let workspace = WorkspaceSpendingCache {
        refreshed_at_ms: published_ms,
        live_baselines: BTreeMap::from([("claude:external-baselined".to_owned(), 1.0)]),
        ..Default::default()
    };

    apply_live_today_spend(&mut snapshot, &workspace);
    assert_eq!(snapshot.today_spend_live_usd, Some(0.0));
}

#[test]
fn current_usage_cost_is_omitted_from_live_workspace_spend() {
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let project = Path::new("/repo/main");
    let mut row = cost_row_at("current-usage", Some(12.0), Some(now), project, None);
    row.as_agent_mut()
        .unwrap()
        .context
        .as_mut()
        .unwrap()
        .cost
        .as_mut()
        .unwrap()
        .coverage = crate::agents::CostCoverage::CurrentUsage;
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(project), Vec::new(), now)
            .with_project_root(Some(project.to_path_buf()));
    let mut antigravity = agent_state("current-usage", project, &transcript("current-usage"), now);
    antigravity.kind = crate::ids::AgentKind::new_unchecked("antigravity");
    snapshot.agents = vec![antigravity].into();
    snapshot.worktree_groups = vec![worktree_group(project, vec![row])].into();

    assert!(live_card_sessions(&snapshot).is_empty());
    apply_live_today_spend(&mut snapshot, &WorkspaceSpendingCache::default());
    assert_eq!(snapshot.today_spend_live_usd, Some(0.0));
}

#[test]
fn session_covered_local_cost_is_included_in_live_workspace_spend() {
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let project = Path::new("/repo/main");
    let mut row = cost_row_at("priced", Some(12.0), Some(now), project, None);
    row.as_agent_mut()
        .unwrap()
        .context
        .as_mut()
        .unwrap()
        .cost
        .as_mut()
        .unwrap()
        .coverage = crate::agents::CostCoverage::Session;
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(project), Vec::new(), now)
            .with_project_root(Some(project.to_path_buf()));
    let mut droid = agent_state(
        "priced",
        project,
        Path::new("/tmp/droid/priced.settings.json"),
        now,
    );
    droid.kind = crate::ids::AgentKind::new_unchecked("droid");
    snapshot.agents = vec![droid].into();
    snapshot.worktree_groups = vec![worktree_group(project, vec![row])].into();

    let cards = live_card_sessions(&snapshot);
    assert_eq!(cards.len(), 1);
    let workspace = WorkspaceSpendingCache {
        refreshed_at_ms: now.as_millisecond() as u64,
        ..WorkspaceSpendingCache::default()
    };
    apply_live_today_spend(&mut snapshot, &workspace);
    assert_eq!(snapshot.today_spend_live_usd, Some(12.0));
}

#[test]
fn claude_live_addback_uses_its_session_not_its_project_folder() {
    let dir = tempfile::tempdir().unwrap();
    let transcripts = dir.path().join("projects/slug");
    std::fs::create_dir_all(&transcripts).unwrap();
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let wt = Path::new("/repo/wt");
    let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let sibling = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let adapter = find_definition("claude").unwrap();
    let files = [(id, 1.0), (sibling, 9.0)]
        .into_iter()
        .map(|(session, usd)| {
            let path = transcripts.join(format!("{session}.jsonl"));
            let line = serde_json::json!({
                "timestamp": published.to_string(),
                "cwd": wt,
                "costUSD": usd,
                "message": {"id": session, "usage": {"input_tokens": 10}}
            });
            std::fs::write(&path, format!("{line}\n")).unwrap();
            SpendingFile {
                adapter,
                login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
                    adapter.spec().kind,
                )),
                path,
            }
        })
        .collect::<Vec<_>>();
    let scope = SpendScope::from_roots(Some(wt), &[]);
    let result = SpendingWalker::new().walk_local(
        &dir.path().join("spending.json"),
        &WalkRequest {
            history: &crate::disk::paths::SpendingHistoryPaths::under(dir.path()),
            files: &files,
            prices: &crate::agents::pricing::PriceBook::default(),
            now_secs: published.as_second() as u64,
            origin_overrides: &Default::default(),
            user_inputs: &[],
            scope: Some(&scope),
            spec: &HeadlineSpec {
                mode: SpendWindowMode::Trailing24h,
                timezone: Some("UTC".into()),
            },
        },
        &mut SilentWalk,
    );
    let workspace = WorkspaceSpendingCache {
        tally: result.workspace.tally,
        day: result.workspace.day,
        headline_cutoff_secs: result.workspace.headline_cutoff_secs,
        live_baselines: result.workspace.live_baselines,
        ..Default::default()
    };
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(wt), Vec::new(), published)
            .with_project_root(Some(wt.to_path_buf()));
    snapshot.agents = vec![agent_state(id, wt, &files[0].path, published)].into();
    snapshot.worktree_groups = vec![worktree_group(wt, vec![cost_row(id, Some(2.0), None)])].into();

    apply_live_today_spend(&mut snapshot, &workspace);
    apply_live_day_spend(&mut snapshot, &workspace);
    assert_eq!(snapshot.today_spend_live_usd, Some(11.0));
    assert_eq!(snapshot.fleet_day_spend_usd, Some(11.0));
    let keys = live_card_sessions(&snapshot).remove(0).session_keys;
    assert_eq!(keys, vec![format!("claude:{id}")]);
    assert_eq!(workspace.live_baselines[&keys[0]], 1.0);
    assert_eq!(workspace.live_baselines[&format!("claude:{sibling}")], 9.0);
}

#[test]
fn claude_fork_live_addback_does_not_recount_copied_history() {
    let dir = tempfile::tempdir().unwrap();
    let transcripts = dir.path().join("projects/slug");
    std::fs::create_dir_all(&transcripts).unwrap();
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let wt = Path::new("/repo/wt");
    let source = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let fork = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let adapter = find_definition("claude").unwrap();
    let line = |message, request, usd| {
        serde_json::json!({
            "timestamp": published.to_string(),
            "cwd": wt,
            "costUSD": usd,
            "requestId": request,
            "message": {"id": message, "usage": {"input_tokens": 10}}
        })
        .to_string()
    };
    let shared = line("msg-shared", "req-shared", 9.0);
    let mut files = [
        (source, vec![shared.clone()]),
        (fork, vec![shared, line("msg-own", "req-own", 1.0)]),
    ]
    .into_iter()
    .map(|(session, lines)| {
        let path = transcripts.join(format!("{session}.jsonl"));
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        SpendingFile {
            adapter,
            login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
                adapter.spec().kind,
            )),
            path,
        }
    })
    .collect::<Vec<_>>();
    let scope = SpendScope::from_roots(Some(wt), &[]);
    for _ in 0..2 {
        let result = SpendingWalker::new().walk_local(
            &dir.path().join("spending.json"),
            &WalkRequest {
                history: &crate::disk::paths::SpendingHistoryPaths::under(dir.path()),
                files: &files,
                prices: &crate::agents::pricing::PriceBook::default(),
                now_secs: published.as_second() as u64,
                origin_overrides: &Default::default(),
                user_inputs: &[],
                scope: Some(&scope),
                spec: &HeadlineSpec {
                    mode: SpendWindowMode::Trailing24h,
                    timezone: Some("UTC".into()),
                },
            },
            &mut SilentWalk,
        );
        assert_eq!(result.spending.total.headline.usd, 10.0);
        let workspace = WorkspaceSpendingCache {
            tally: result.workspace.tally,
            day: result.workspace.day,
            headline_cutoff_secs: result.workspace.headline_cutoff_secs,
            live_baselines: result.workspace.live_baselines,
            ..Default::default()
        };
        for source_is_live in [false, true] {
            let mut snapshot =
                SidebarSnapshot::build(WorkspaceId::from_project_root(wt), Vec::new(), published)
                    .with_project_root(Some(wt.to_path_buf()));
            let mut agents = vec![agent_state(
                fork,
                wt,
                &transcripts.join(format!("{fork}.jsonl")),
                published,
            )];
            let mut rows = vec![cost_row(fork, Some(10.0), None)];
            if source_is_live {
                agents.push(agent_state(
                    source,
                    wt,
                    &transcripts.join(format!("{source}.jsonl")),
                    published,
                ));
                rows.push(cost_row(source, Some(9.0), None));
            }
            snapshot.agents = agents.into();
            snapshot.worktree_groups = vec![worktree_group(wt, rows)].into();
            apply_live_today_spend(&mut snapshot, &workspace);
            apply_live_day_spend(&mut snapshot, &workspace);
            assert_eq!(snapshot.today_spend_live_usd, Some(10.0));
            assert_eq!(snapshot.fleet_day_spend_usd, Some(10.0));
        }
        assert_eq!(
            workspace.live_baselines,
            BTreeMap::from([
                (format!("claude:{source}"), 9.0),
                (format!("claude:{fork}"), 10.0),
            ])
        );
        files.reverse();
    }
}

fn walk_agy_session(
    dir: &Path,
    id: &str,
    rows: &[(u32, &str, u64)],
    mode: SpendWindowMode,
) -> (PathBuf, WorkspaceSpendingCache) {
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let adapter = find_definition("antigravity").unwrap();
    let logs = dir.join(format!("brain/{id}/.system_generated/logs"));
    std::fs::create_dir_all(&logs).unwrap();
    let path = logs.join("transcript_full.jsonl");
    let lines = rows.iter().map(|(step, at, tokens)| {
        serde_json::json!({
            "step_index": step,
            "type": "PLANNER_RESPONSE",
            "created_at": at,
            "model": "agy-priced",
            "input_tokens": tokens,
            "output_tokens": 0
        })
        .to_string()
    });
    std::fs::write(&path, lines.collect::<Vec<_>>().join("\n") + "\n").unwrap();
    let files = [SpendingFile {
        adapter,
        login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
            adapter.spec().kind,
        )),
        path: path.clone(),
    }];
    let prices = crate::agents::pricing::PriceBook::from_litellm_json(
        r#"{"agy-priced":{"input_cost_per_token":1e-6,"output_cost_per_token":1e-6}}"#,
    );
    let scope = SpendScope::from_roots(Some(Path::new("/repo/wt")), &[]);
    let result = SpendingWalker::new().walk_local(
        &dir.join("spending.json"),
        &WalkRequest {
            history: &crate::disk::paths::SpendingHistoryPaths::under(dir),
            files: &files,
            prices: &prices,
            now_secs: published.as_second() as u64,
            origin_overrides: &Default::default(),
            user_inputs: &[],
            scope: Some(&scope),
            spec: &HeadlineSpec {
                mode,
                timezone: Some("UTC".into()),
            },
        },
        &mut SilentWalk,
    );
    let workspace = WorkspaceSpendingCache {
        tally: result.workspace.tally,
        day: result.workspace.day,
        headline_cutoff_secs: result.workspace.headline_cutoff_secs,
        live_baselines: result.workspace.live_baselines,
        headline_live_baselines: result.workspace.headline_live_baselines,
        ..Default::default()
    };
    (path, workspace)
}

#[test]
fn agy_live_addback_counts_only_today_of_a_session_begun_yesterday() {
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let wt = Path::new("/repo/wt");
    let id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let rows = [
        (2, "2025-06-14T23:00:00Z", 20_000_000),
        (4, "2025-06-15T10:00:00Z", 100_000),
    ];
    let (today_dir, trailing_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (path, workspace) = walk_agy_session(today_dir.path(), id, &rows, SpendWindowMode::Today);
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(wt), Vec::new(), published)
            .with_project_root(Some(wt.to_path_buf()));
    let mut agy = agent_state(id, wt, &path, published);
    agy.kind = crate::ids::AgentKind::new_unchecked("antigravity");
    snapshot.agents = vec![agy].into();
    let mut row = cost_row(id, Some(20.1), None);
    row.as_agent_mut()
        .unwrap()
        .context
        .as_mut()
        .unwrap()
        .cost
        .as_mut()
        .unwrap()
        .coverage = crate::agents::CostCoverage::Session;
    snapshot.worktree_groups = vec![worktree_group(wt, vec![row])].into();

    apply_live_day_spend(&mut snapshot, &workspace);
    let day = snapshot.fleet_day_spend_usd.unwrap();
    assert!(
        (day - 0.1).abs() < 1e-6,
        "yesterday's agy rows count today: {day}"
    );
    // A trailing 24 h headline still holds yesterday evening's row.
    let (_, trailing) =
        walk_agy_session(trailing_dir.path(), id, &rows, SpendWindowMode::Trailing24h);
    apply_live_today_spend(&mut snapshot, &trailing);
    let headline = snapshot.today_spend_live_usd.unwrap();
    assert!(
        (headline - 20.1).abs() < 1e-6,
        "the 24 h headline counts the whole session: {headline}"
    );
}

#[test]
fn an_old_agy_session_leaves_no_live_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let id = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    // Twenty days before the walk: inside the year, outside the raw window.
    let rows = [(2, "2025-05-26T10:00:00Z", 1_000_000)];
    let (_, workspace) = walk_agy_session(dir.path(), id, &rows, SpendWindowMode::Today);
    assert!(
        workspace.live_baselines.is_empty(),
        "{:?}",
        workspace.live_baselines
    );
}

#[test]
fn agy_split_walk_baselines_match_cold_walk() {
    use std::io::Write as _;

    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let wt = Path::new("/repo/wt");
    let id = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let adapter = find_definition("antigravity").unwrap();
    let prices = crate::agents::pricing::PriceBook::from_litellm_json(
        r#"{"agy-priced":{"input_cost_per_token":1e-6,"output_cost_per_token":1e-6}}"#,
    );
    let model_row = |step: u32, at: &str, input: u64, output: u64| {
        serde_json::json!({
            "step_index": step,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": at,
            "model": "agy-priced",
            "input_tokens": input,
            "output_tokens": output,
            "tool_calls": [{"name": "run_command", "args": {}}]
        })
        .to_string()
    };
    let generic_row = |step: u32, at: &str| {
        serde_json::json!({
            "step_index": step,
            "source": "MODEL",
            "type": "GENERIC",
            "status": "DONE",
            "created_at": at,
            "content": "ok"
        })
        .to_string()
    };
    // Row 1 precedes both the UTC day cutoff (2025-06-15T00:00Z) and the trailing 24h cutoff
    // (2025-06-14T10:26:40Z); row 3 precedes the day cutoff only; row 5 is today.
    let row1 = model_row(1, "2025-06-14T08:00:00Z", 1_000_000, 200);
    let follower1 = generic_row(2, "2025-06-14T08:00:05Z");
    let row3 = model_row(3, "2025-06-14T23:00:00Z", 2_000_000, 200);
    let follower3 = generic_row(4, "2025-06-14T23:00:05Z");
    let row5 = model_row(5, "2025-06-15T09:00:00Z", 500_000, 100);
    let scope = SpendScope::from_roots(Some(wt), &[]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Trailing24h,
        timezone: Some("UTC".into()),
    };

    let split_dir = tempfile::tempdir().unwrap();
    let split_logs = split_dir
        .path()
        .join(format!("brain/{id}/.system_generated/logs"));
    std::fs::create_dir_all(&split_logs).unwrap();
    let split_path = split_logs.join("transcript_full.jsonl");
    let split_files = [SpendingFile {
        adapter,
        login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
            adapter.spec().kind,
        )),
        path: split_path.clone(),
    }];
    let split_cache = split_dir.path().join("spending.json");
    let split_history = crate::disk::paths::SpendingHistoryPaths::under(split_dir.path());
    let mut split_walker = SpendingWalker::new();

    std::fs::write(&split_path, format!("{row1}\n")).unwrap();
    let walk = |walker: &mut SpendingWalker,
                cache: &Path,
                history: &crate::disk::paths::SpendingHistoryPaths,
                files: &[SpendingFile]| {
        walker.walk_local(
            cache,
            &WalkRequest {
                history,
                files,
                prices: &prices,
                now_secs: published.as_second() as u64,
                origin_overrides: &Default::default(),
                user_inputs: &[],
                scope: Some(&scope),
                spec: &spec,
            },
            &mut SilentWalk,
        )
    };
    walk(
        &mut split_walker,
        &split_cache,
        &split_history,
        &split_files,
    );
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&split_path)
            .unwrap();
        writeln!(file, "{follower1}").unwrap();
        writeln!(file, "{row3}").unwrap();
    }
    walk(
        &mut split_walker,
        &split_cache,
        &split_history,
        &split_files,
    );
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&split_path)
            .unwrap();
        writeln!(file, "{follower3}").unwrap();
        writeln!(file, "{row5}").unwrap();
    }
    let split_result = walk(
        &mut split_walker,
        &split_cache,
        &split_history,
        &split_files,
    );

    let cold_dir = tempfile::tempdir().unwrap();
    let cold_logs = cold_dir
        .path()
        .join(format!("brain/{id}/.system_generated/logs"));
    std::fs::create_dir_all(&cold_logs).unwrap();
    let cold_path = cold_logs.join("transcript_full.jsonl");
    std::fs::write(
        &cold_path,
        format!("{row1}\n{follower1}\n{row3}\n{follower3}\n{row5}\n"),
    )
    .unwrap();
    let cold_files = [SpendingFile {
        adapter,
        login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
            adapter.spec().kind,
        )),
        path: cold_path,
    }];
    let cold_cache = cold_dir.path().join("spending.json");
    let cold_history = crate::disk::paths::SpendingHistoryPaths::under(cold_dir.path());
    let mut cold_walker = SpendingWalker::new();
    let cold_result = walk(&mut cold_walker, &cold_cache, &cold_history, &cold_files);

    assert_eq!(
        split_result.workspace.live_baselines, cold_result.workspace.live_baselines,
        "split walk live_baselines must match cold walk"
    );
    assert_eq!(
        split_result.workspace.headline_live_baselines,
        cold_result.workspace.headline_live_baselines,
        "split walk headline_live_baselines must match cold walk"
    );
    let key = format!("antigravity:{id}");
    assert!((cold_result.workspace.live_baselines[&key] - 3.0004).abs() < 1e-9);
    assert!((cold_result.workspace.headline_live_baselines[&key] - 1.0002).abs() < 1e-9);
}

#[test]
fn originless_keyless_rows_each_count_once_in_baselines() {
    let published = Timestamp::from_second(1_750_000_000).unwrap();
    let wt = Path::new("/repo/wt");
    let adapter = find_definition("pi").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    // No `session` header means no origin; two keyless assistant rows before the UTC day cutoff
    // (2025-06-15T00:00Z), even with identical payloads, must each count once.
    let line = serde_json::json!({
        "type": "message",
        "timestamp": "2025-06-14T23:00:00.000Z",
        "message": {
            "role": "assistant",
            "model": "gpt-5",
            "usage": {"input": 100, "output": 50, "cost": {"total": 1.0}}
        }
    })
    .to_string();
    std::fs::write(&path, format!("{line}\n{line}\n")).unwrap();
    let files = [SpendingFile {
        adapter,
        login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
            adapter.spec().kind,
        )),
        path,
    }];
    let scope = SpendScope::from_roots(Some(wt), &[]);
    let result = SpendingWalker::new().walk_local(
        &dir.path().join("spending.json"),
        &WalkRequest {
            history: &crate::disk::paths::SpendingHistoryPaths::under(dir.path()),
            files: &files,
            prices: &crate::agents::pricing::PriceBook::default(),
            now_secs: published.as_second() as u64,
            origin_overrides: &Default::default(),
            user_inputs: &[],
            scope: Some(&scope),
            spec: &HeadlineSpec {
                mode: SpendWindowMode::Today,
                timezone: Some("UTC".into()),
            },
        },
        &mut SilentWalk,
    );
    let total: f64 = result.workspace.live_baselines.values().sum();
    assert!(
        (total - 2.0).abs() < 1e-9,
        "keyless originless rows must each count once: {:?}",
        result.workspace.live_baselines
    );
}
