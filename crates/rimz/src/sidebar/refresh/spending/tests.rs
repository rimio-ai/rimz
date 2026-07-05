use crate::RuntimePaths;
use crate::SidebarSnapshot;
use crate::agents::TurnPhase;
use crate::agents::spending::{
    CachedEntry, FileCacheEntry, HeadlineSpec, PROVIDER_SPENDING_VERSION, ProviderSpendingCache,
    SpendCursor, SpendScope, SpendWindowMode, Spending, SpendingWalker,
    override_discovered_spending_files_for_test, read_provider_spending_cache, read_spending_cache,
    read_workspace_spending_cache, unix_secs_now, utc_date, write_provider_spending_cache,
    write_spending_cache, write_workspace_spending_cache,
};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::AgentKind;
use crate::ids::WorkspaceId;
use crate::ledger::single_flight::{Coalesced, coalesce};
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{SPENDING_STALE_GRACE, SPENDING_TTL};

use jiff::Timestamp;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use super::{
    codex_origin_overrides, compute_fleet_spending_with_walker, workspace_cache_from_shared_entries,
};

fn compute_fleet_spending(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &HeadlineSpec,
) -> crate::agents::spending::SpendingCaches {
    let mut walker = SpendingWalker::new();
    compute_fleet_spending_with_walker(&mut walker, runtime, snapshot, spec)
}

fn cached_entry(ts_secs: u64, cost_usd: f64, id: &str) -> CachedEntry {
    CachedEntry {
        ts_secs,
        cost_usd,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        message_id: Some(format!("msg-{id}")),
        request_id: Some(format!("req-{id}")),
        thread_id: Some(format!("thread-{id}")),
        is_sidechain: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    }
}

fn file_cache_entry(path: &std::path::Path, entries: Vec<CachedEntry>) -> FileCacheEntry {
    let metadata = std::fs::metadata(path).expect("transcript metadata");
    let mtime_secs = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    FileCacheEntry {
        mtime_secs,
        len: metadata.len(),
        cursor: SpendCursor::default(),
        origin_path: None,
        entries,
        unknown_models: BTreeMap::new(),
    }
}

fn claude_cost_line(ts_secs: u64, cost_usd: f64, id: &str) -> String {
    let tod = ts_secs % 86_400;
    format!(
        r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","costUSD":{cost_usd:.2},"requestId":"req-{id}","message":{{"id":"msg-{id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#,
        utc_date(ts_secs),
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60
    )
}

fn claude_cost_line_in(ts_secs: u64, cost_usd: f64, id: &str, cwd: &std::path::Path) -> String {
    let tod = ts_secs % 86_400;
    format!(
        r#"{{"timestamp":"{}T{:02}:{:02}:{:02}.000Z","cwd":"{}","costUSD":{cost_usd:.2},"requestId":"req-{id}","message":{{"id":"msg-{id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#,
        utc_date(ts_secs),
        tod / 3_600,
        (tod % 3_600) / 60,
        tod % 60,
        cwd.display()
    )
}

fn hold_shared_spending_lock(
    runtime: &RuntimePaths,
) -> crate::ledger::single_flight::ProducerGuard {
    match coalesce::<()>(&runtime.shared_spending_lock(), Duration::ZERO, 1, || None) {
        Coalesced::Produce(guard) => guard,
        _ => panic!("test must hold the global spending lock"),
    }
}

#[test]
fn fresh_shared_publish_returns_without_walking() {
    let dir = tempfile::tempdir().unwrap();
    let first = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime paths");
    let second = RuntimePaths::under(
        WorkspaceId::from_project_root(&dir.path().join("other")),
        dir.path(),
    )
    .expect("runtime paths");
    first.ensure_dirs().expect("runtime dirs");
    second.ensure_dirs().expect("runtime dirs");
    let published_at = unix_now_ms();
    let mut spending = Spending::default();
    spending.total.headline.usd = 1.23;
    write_provider_spending_cache(
        &first.shared_provider_spending_path(),
        published_at,
        &spending,
    );
    let snapshot = SidebarSnapshot::build(
        second.workspace_id.clone(),
        Vec::new(),
        Vec::new(),
        Timestamp::now(),
    );

    let cache = compute_fleet_spending(&second, &snapshot, &HeadlineSpec::default());

    assert_eq!(cache.provider.refreshed_at_ms, published_at);
    assert_eq!(cache.provider.spending, spending);
}

#[test]
fn shared_spending_lock_serves_the_elected_publish_to_a_contender() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");

    let guard = match coalesce::<crate::agents::spending::ProviderSpendingCache>(
        &runtime.shared_spending_lock(),
        Duration::ZERO,
        1,
        || None,
    ) {
        Coalesced::Produce(guard) => guard,
        _ => panic!("first contender must hold the shared spending election"),
    };

    let published_at = unix_now_ms();
    let mut spending = Spending::default();
    spending.total.headline.usd = 4.56;
    let polls = AtomicU32::new(0);
    let outcome = coalesce(&runtime.shared_spending_lock(), Duration::ZERO, 3, || {
        if polls.fetch_add(1, Ordering::SeqCst) == 1 {
            write_provider_spending_cache(
                &runtime.shared_provider_spending_path(),
                published_at,
                &spending,
            );
        }
        let cache = crate::agents::spending::read_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
        );
        cache.is_fresh(unix_now_ms()).then_some(cache)
    });

    drop(guard);
    let Coalesced::Shared(cache) = outcome else {
        panic!("a contender must consume the elected producer's publish");
    };
    assert_eq!(cache.refreshed_at_ms, published_at);
    assert_eq!(cache.spending, spending);
}

#[test]
fn produce_local_serves_published_within_grace() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let transcript = dir.path().join("claude.jsonl");
    std::fs::write(
        &transcript,
        claude_cost_line(unix_secs_now(), 9.0, "deleted"),
    )
    .expect("transcript");
    let _discovered = override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript.clone(),
    )]);
    std::fs::remove_file(&transcript).expect("delete transcript");

    let now_ms = unix_now_ms();
    let published_at = now_ms.saturating_sub(SPENDING_TTL.as_millis() as u64 + 1_000);
    let mut spending = Spending::default();
    spending.total.headline.usd = 4.56;
    spending.total.year.usd = 4.56;
    write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        published_at,
        &spending,
    );
    let _held = hold_shared_spending_lock(&runtime);
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    let mut walker = SpendingWalker::new();

    let caches = compute_fleet_spending_with_walker(
        &mut walker,
        &runtime,
        &snapshot,
        &HeadlineSpec::default(),
    );

    assert_eq!(caches.provider.refreshed_at_ms, published_at);
    assert_eq!(caches.provider.spending, spending);
}

#[test]
fn produce_local_walk_seeds_from_cursor_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = unix_secs_now();
    std::fs::write(&transcript, claude_cost_line(now_secs, 9.0, "live")).expect("transcript");
    let _discovered = override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript.clone(),
    )]);

    let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        file_cache_entry(&transcript, vec![cached_entry(now_secs, 2.25, "cached")]),
    );
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);

    let mut stale = Spending::default();
    stale.total.headline.usd = 99.0;
    let stale_at = unix_now_ms().saturating_sub(SPENDING_STALE_GRACE.as_millis() as u64 + 1_000);
    write_provider_spending_cache(&runtime.shared_provider_spending_path(), stale_at, &stale);
    let _held = hold_shared_spending_lock(&runtime);
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    let mut walker = SpendingWalker::new();

    let caches = compute_fleet_spending_with_walker(
        &mut walker,
        &runtime,
        &snapshot,
        &HeadlineSpec::default(),
    );

    assert!((caches.provider.spending.total.headline.usd - 2.25).abs() < 1e-9);
    assert!((caches.provider.spending.total.year.usd - 2.25).abs() < 1e-9);
    assert_eq!(
        read_provider_spending_cache(&runtime.shared_provider_spending_path()).refreshed_at_ms,
        stale_at,
        "local fallback does not publish while another producer owns the lock"
    );
}

#[test]
fn walk_local_stays_memory_only_while_publishing_walk_writes_provider_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = unix_secs_now();
    std::fs::write(&transcript, claude_cost_line(now_secs, 2.5, "publish")).expect("transcript");
    let _discovered = override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )]);
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    let spec = HeadlineSpec::default();

    let mut local_walker = SpendingWalker::new();
    let local = super::walk_fleet_spending(&mut local_walker, &runtime, &snapshot, &spec, false);

    assert!((local.provider.spending.total.year.usd - 2.5).abs() < 1e-9);
    assert_eq!(
        read_provider_spending_cache(&runtime.shared_provider_spending_path()).refreshed_at_ms,
        0,
        "local fallback must not publish provider-spending.json"
    );

    let mut publishing_walker = SpendingWalker::new();
    let published =
        super::walk_fleet_spending(&mut publishing_walker, &runtime, &snapshot, &spec, true);
    let on_disk = read_provider_spending_cache(&runtime.shared_provider_spending_path());

    assert_eq!(on_disk.version, PROVIDER_SPENDING_VERSION);
    assert!((published.provider.spending.total.year.usd - 2.5).abs() < 1e-9);
    assert!((on_disk.spending.total.year.usd - 2.5).abs() < 1e-9);
}

#[test]
fn walk_local_reconciles_workspace_carry_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let project = dir.path().join("repo");
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = unix_secs_now();
    std::fs::write(
        &transcript,
        claude_cost_line_in(now_secs, 2.5, "local", &project),
    )
    .expect("transcript");
    let _discovered = override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )]);
    let scope = SpendScope::for_workspace(Some(&project), &[], None);
    let scope_hash = scope.hash();
    let mut previous_tally = crate::agents::spending::SpendTally::default();
    previous_tally.headline.usd = 2.0;
    let previous = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: 10_000,
        scope_hash: scope_hash.clone(),
        tally: previous_tally,
        headline_cutoff_secs: now_secs / 86_400 * 86_400,
        carry_usd: 1.0,
        ..Default::default()
    };
    let workspace_path = runtime.workspace_spending_path(&scope_hash);
    write_workspace_spending_cache(&workspace_path, &previous);
    let before_bytes = std::fs::read(&workspace_path).expect("workspace cache");
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_project_root(Some(project));
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let mut walker = SpendingWalker::new();

    let local = super::walk_fleet_spending(&mut walker, &runtime, &snapshot, &spec, false);

    assert!((local.workspace.tally.headline.usd - 2.5).abs() < 1e-9);
    assert!((local.workspace.carry_usd - 0.5).abs() < 1e-9);
    assert_eq!(
        std::fs::read(&workspace_path).expect("workspace cache"),
        before_bytes,
        "local fallback computes carry for its own frame without publishing"
    );
}

#[test]
fn publishing_walk_observer_checkpoints_workspace_carry_and_baselines() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("repo");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&project), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scope_hash = scope.hash();
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = unix_secs_now();
    let raw = {
        let mut cache = read_spending_cache(&runtime.shared_spending_cursor_path());
        cache.files.insert(
            transcript.to_string_lossy().into_owned(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 1,
                cursor: SpendCursor::default(),
                origin_path: Some(project),
                entries: vec![CachedEntry {
                    ts_secs: now_secs,
                    cost_usd: 1.25,
                    input: 10,
                    output: 5,
                    cache_write: 0,
                    cache_read: 0,
                    message_id: Some("msg-1".to_owned()),
                    request_id: Some("req-1".to_owned()),
                    thread_id: None,
                    is_sidechain: false,
                    model: None,
                    rolled: false,
                }],
                unknown_models: BTreeMap::new(),
            },
        );
        cache
    };
    let mut previous_tally = crate::agents::spending::SpendTally::default();
    previous_tally.headline.usd = 1.0;
    write_workspace_spending_cache(
        &runtime.workspace_spending_path(&scope_hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: 10_000,
            scope_hash: scope_hash.clone(),
            tally: previous_tally,
            headline_cutoff_secs: now_secs / 86_400 * 86_400,
            carry_usd: 0.5,
            live_baselines: BTreeMap::from([("agent".to_owned(), 5.0)]),
            ..Default::default()
        },
    );
    let files = vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let live_costs = vec![("agent".to_owned(), 7.0, Some(1_000))];
    let provider_path = runtime.shared_provider_spending_path();
    let automation_files = HashSet::new();
    let mut observer = super::PublishingWalkObserver {
        runtime: &runtime,
        provider_path,
        files: &files,
        automation_files: &automation_files,
        now_secs,
        scope: Some(&scope),
        scope_hash: Some(scope_hash.clone()),
        spec: &spec,
        live_costs: &live_costs,
    };

    crate::agents::spending::WalkObserver::on_interval(&mut observer, &raw);

    let workspace = read_workspace_spending_cache(&runtime.workspace_spending_path(&scope_hash));
    assert!((workspace.tally.headline.usd - 1.25).abs() < 1e-9);
    assert!((workspace.carry_usd - 2.25).abs() < 1e-9);
    assert_eq!(
        workspace.live_baselines,
        BTreeMap::from([("agent".to_owned(), 7.0)])
    );
}

#[test]
fn workspace_cache_derives_from_shared_entries_while_global_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let other_project = dir.path().join("other");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&other_project), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");

    let project = dir.path().join("repo");
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scope_hash = scope.hash();
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = unix_secs_now();
    let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        FileCacheEntry {
            mtime_secs: 1,
            len: 1,
            cursor: SpendCursor::default(),
            origin_path: Some(project.clone()),
            entries: vec![CachedEntry {
                ts_secs: now_secs,
                cost_usd: 1.25,
                input: 10,
                output: 5,
                cache_write: 0,
                cache_read: 0,
                message_id: Some("msg-1".to_owned()),
                request_id: Some("req-1".to_owned()),
                thread_id: None,
                is_sidechain: false,
                model: None,
                rolled: false,
            }],
            unknown_models: BTreeMap::new(),
        },
    );
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);

    let mut spending = Spending::default();
    spending.total.headline.usd = 1.25;
    spending.total.year.usd = 1.25;
    let provider = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: unix_now_ms(),
        spending,
        ..ProviderSpendingCache::default()
    };

    let _held = match coalesce::<()>(&runtime.shared_spending_lock(), Duration::ZERO, 1, || None) {
        Coalesced::Produce(guard) => guard,
        _ => panic!("test must hold the global spending lock"),
    };
    let files = vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )];
    let stale_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(stale_hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: unix_now_ms(),
            scope_hash: stale_hash.to_owned(),
            ..Default::default()
        },
    );
    assert!(runtime.workspace_spending_path(stale_hash).exists());

    let workspace = workspace_cache_from_shared_entries(
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &HeadlineSpec::default(),
        &[],
    )
    .expect("workspace cache derives from the shared cursor cache");

    assert!((workspace.tally.headline.usd - 1.25).abs() < 1e-9);
    assert_eq!(
        read_workspace_spending_cache(&runtime.workspace_spending_path(&scope_hash)).tally,
        workspace.tally
    );
    assert!(
        !runtime.workspace_spending_path(stale_hash).exists(),
        "producer publishing the current scope prunes old per-hash workspace caches"
    );
}

#[test]
fn workspace_cache_from_shared_entries_reconciles_carry_and_baselines() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("repo");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&project), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scope_hash = scope.hash();
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = unix_secs_now();
    let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        FileCacheEntry {
            mtime_secs: 1,
            len: 1,
            cursor: SpendCursor::default(),
            origin_path: Some(project.clone()),
            entries: vec![CachedEntry {
                ts_secs: now_secs,
                cost_usd: 1.25,
                input: 10,
                output: 5,
                cache_write: 0,
                cache_read: 0,
                message_id: Some("msg-1".to_owned()),
                request_id: Some("req-1".to_owned()),
                thread_id: None,
                is_sidechain: false,
                model: None,
                rolled: false,
            }],
            unknown_models: BTreeMap::new(),
        },
    );
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let files = vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let scoped = crate::agents::spending::compute_scoped_spending(
        &files,
        &raw,
        &Default::default(),
        &scope,
        now_secs,
        &spec,
    );
    let mut previous_tally = crate::agents::spending::SpendTally::default();
    previous_tally.headline.usd = 1.0;
    write_workspace_spending_cache(
        &runtime.workspace_spending_path(&scope_hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: 10_000,
            scope_hash: scope_hash.clone(),
            tally: previous_tally,
            headline_cutoff_secs: scoped.headline_cutoff_secs,
            carry_usd: 0.5,
            live_baselines: BTreeMap::from([("agent".to_owned(), 5.0)]),
            ..Default::default()
        },
    );
    let mut spending = Spending::default();
    spending.total.headline.usd = 1.25;
    spending.total.year.usd = 1.25;
    let provider = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: unix_now_ms(),
        spending,
        ..ProviderSpendingCache::default()
    };
    let live_costs = vec![("agent".to_owned(), 7.0, Some(1_000))];

    let workspace = workspace_cache_from_shared_entries(
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &spec,
        &live_costs,
    )
    .expect("workspace cache derives from the shared cursor cache");

    assert!((workspace.tally.headline.usd - 1.25).abs() < 1e-9);
    assert_eq!(workspace.headline_cutoff_secs, scoped.headline_cutoff_secs);
    assert!((workspace.carry_usd - 2.25).abs() < 1e-9);
    assert_eq!(
        workspace.live_baselines,
        BTreeMap::from([("agent".to_owned(), 7.0)])
    );
    let fresh = super::fresh_workspace_cache(&runtime, Some(&scope_hash), unix_now_ms())
        .expect("fresh workspace cache");
    assert_eq!(fresh, workspace);
}

#[test]
fn producer_publishes_compacted_shared_spending_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let transcript = dir.path().join("claude.jsonl");
    std::fs::write(&transcript, b"").expect("transcript");
    let metadata = std::fs::metadata(&transcript).expect("transcript metadata");
    let mtime_secs = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let now_secs = unix_secs_now();
    let old_a = CachedEntry {
        ts_secs: now_secs - 40 * 86_400,
        cost_usd: 1.0,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        message_id: None,
        request_id: None,
        thread_id: Some("old".to_owned()),
        is_sidechain: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    };
    let old_b = CachedEntry {
        cost_usd: 2.0,
        ..old_a.clone()
    };
    let key = transcript.to_string_lossy().into_owned();
    let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        key.clone(),
        FileCacheEntry {
            mtime_secs,
            len: metadata.len(),
            cursor: SpendCursor::default(),
            origin_path: None,
            entries: vec![old_a, old_b],
            unknown_models: BTreeMap::new(),
        },
    );
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let before_len = std::fs::metadata(runtime.shared_spending_cursor_path())
        .expect("seed spending cache")
        .len();
    let _discovered = override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )]);
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());

    let caches = compute_fleet_spending(&runtime, &snapshot, &HeadlineSpec::default());

    assert_eq!(caches.provider.spending.total.year.usd, 3.0);
    let compacted = read_spending_cache(&runtime.shared_spending_cursor_path());
    let entries = &compacted.files[&key].entries;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].rolled);
    assert_eq!(entries[0].cost_usd, 3.0);
    assert!(
        std::fs::metadata(runtime.shared_spending_cursor_path())
            .expect("compacted spending cache")
            .len()
            < before_len,
        "producer writes back the compacted cursor cache"
    );
}

#[test]
fn empty_discovery_preserves_prior_nonzero_provider_publish() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let project = dir.path().join("repo");
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_project_root(Some(project.clone()));
    let scope = SpendScope::for_workspace(Some(&project), &[], None);
    let scope_hash = scope.hash();

    let mut spending = Spending::default();
    spending.total.headline.usd = 981.0;
    spending.total.year.usd = 981.0;
    write_provider_spending_cache(&runtime.shared_provider_spending_path(), 1, &spending);
    let mut workspace_tally = crate::agents::SpendTally::default();
    workspace_tally.headline.usd = 42.0;
    workspace_tally.year.usd = 42.0;
    write_workspace_spending_cache(
        &runtime.workspace_spending_path(&scope_hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: 1,
            scope_hash: scope_hash.clone(),
            tally: workspace_tally,
            ..Default::default()
        },
    );

    let _discovered = override_discovered_spending_files_for_test(Vec::new());
    let cache = compute_fleet_spending(&runtime, &snapshot, &HeadlineSpec::default());

    assert_eq!(
        cache.provider.spending.total.headline.usd, 981.0,
        "a transient empty transcript discovery must not publish a fresh zero"
    );
    assert_eq!(
        cache.workspace.tally.headline.usd, 42.0,
        "the matching workspace cache is kept with the retained provider publish"
    );
    let published = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    assert_eq!(
        published.refreshed_at_ms, 1,
        "the stale non-zero provider cache is returned, not overwritten as fresh"
    );
    assert_eq!(published.spending.total.headline.usd, 981.0);
}

#[test]
fn codex_origin_overrides_read_transcript_and_worktree_from_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("rollout.jsonl");
    let worktree = dir.path().join("repo");
    let now = Timestamp::now();
    let agent = AgentState {
        agent_id: "codex-1".into(),
        kind: AgentKind::new_unchecked("codex"),
        name: None,
        kind_ordinal: None,
        profile: None,
        role: None,
        team: None,
        launch_group: None,
        launch_ordinal: None,
        channel: None,
        status: AgentStatus::Running,
        phase: TurnPhase::Idle,
        pane: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: Some(worktree.display().to_string()),
        worktree_branch: None,
        task: None,
        prompt: None,
        description: None,
        transcript_path: Some(transcript.display().to_string()),
        origin: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_compact_command_tokens: None,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    };
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(&worktree),
        Vec::new(),
        vec![agent],
        now,
    );

    let overrides = codex_origin_overrides(&snapshot);

    assert_eq!(overrides.get(&transcript), Some(&worktree));
}
