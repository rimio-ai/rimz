use crate::RuntimePaths;
use crate::agents::spending::{
    CachedEntry, FileCacheEntry, HeadlineSpec, PROVIDER_SPENDING_VERSION, ProviderSpendingCache,
    SPENDING_STALE_GRACE, SPENDING_TTL, SpendCursor, SpendScope, SpendWindowMode, Spending,
    SpendingWalker, override_discovered_spending_files_for_test, read_provider_spending_cache,
    read_spending_cache, read_workspace_spending_cache, unix_now_ms, unix_secs_now, utc_date,
    write_provider_spending_cache, write_spending_cache, write_workspace_spending_cache,
};
use crate::ids::WorkspaceId;
use crate::store::single_flight::{Coalesced, coalesce};

use jiff::Timestamp;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use super::workspace_cache_from_shared_entries;

fn compute_fleet_spending(
    runtime: &RuntimePaths,
    project_root: Option<&std::path::Path>,
    spec: &HeadlineSpec,
) -> crate::agents::spending::SpendingCaches {
    let mut walker = SpendingWalker::new();
    compute_fleet_spending_with_walker(&mut walker, runtime, project_root, spec)
}

fn compute_fleet_spending_with_walker(
    walker: &mut SpendingWalker,
    runtime: &RuntimePaths,
    project_root: Option<&std::path::Path>,
    spec: &HeadlineSpec,
) -> crate::agents::spending::SpendingCaches {
    super::serve_direct(
        walker,
        runtime,
        &service_request(runtime, project_root, spec),
    )
}

fn walk_fleet_spending(
    walker: &mut SpendingWalker,
    runtime: &RuntimePaths,
    project_root: Option<&std::path::Path>,
    spec: &HeadlineSpec,
    publish: bool,
) -> crate::agents::spending::SpendingCaches {
    super::walk_request_for_test(
        walker,
        runtime,
        &service_request(runtime, project_root, spec),
        publish,
    )
}

fn service_request(
    runtime: &RuntimePaths,
    project_root: Option<&std::path::Path>,
    spec: &HeadlineSpec,
) -> crate::agents::spending::service::SpendingServiceRequest {
    crate::agents::spending::service::SpendingServiceRequest::workspace(
        runtime,
        runtime.workspace_id.clone(),
        project_root.map(std::path::Path::to_path_buf),
        Vec::new(),
        None,
        HashMap::new(),
        spec.clone(),
    )
}

fn cached_entry(ts_secs: u64, cost_usd: f64, id: &str) -> CachedEntry {
    CachedEntry {
        ts_secs,
        cost_usd,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        tool_calls: Default::default(),
        message_id: Some(format!("msg-{id}")),
        request_id: Some(format!("req-{id}")),
        dedup_key: None,
        thread_id: Some(format!("thread-{id}")),
        is_sidechain: false,
        has_speed: false,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: false,
    }
}

fn file_cache_entry(path: &std::path::Path, entries: Vec<CachedEntry>) -> FileCacheEntry {
    FileCacheEntry {
        stat: crate::agents::TranscriptStat::from_path(path).expect("transcript stat"),
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

fn hold_shared_spending_lock(runtime: &RuntimePaths) -> crate::store::single_flight::ProducerGuard {
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
    let cache = compute_fleet_spending(&second, None, &HeadlineSpec::default());

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
        crate::agents::definition_by_kind("claude").unwrap(),
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
    let mut walker = SpendingWalker::new();

    let caches =
        compute_fleet_spending_with_walker(&mut walker, &runtime, None, &HeadlineSpec::default());

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
        crate::agents::definition_by_kind("claude").unwrap(),
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
    let mut walker = SpendingWalker::new();

    let caches = compute_fleet_spending_with_walker(
        &mut walker,
        &runtime,
        None,
        &HeadlineSpec {
            mode: SpendWindowMode::Today,
            timezone: Some("UTC".to_owned()),
        },
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
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript,
    )]);
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };

    let mut local_walker = SpendingWalker::new();
    let local = walk_fleet_spending(&mut local_walker, &runtime, None, &spec, false);

    assert!((local.provider.spending.total.year.usd - 2.5).abs() < 1e-9);
    assert_eq!(
        read_provider_spending_cache(&runtime.shared_provider_spending_path()).refreshed_at_ms,
        0,
        "local fallback must not publish provider-spending.json"
    );

    let mut publishing_walker = SpendingWalker::new();
    let published = walk_fleet_spending(&mut publishing_walker, &runtime, None, &spec, true);
    let on_disk = read_provider_spending_cache(&runtime.shared_provider_spending_path());

    assert_eq!(on_disk.version, PROVIDER_SPENDING_VERSION);
    assert!((published.provider.spending.total.year.usd - 2.5).abs() < 1e-9);
    assert!((on_disk.spending.total.year.usd - 2.5).abs() < 1e-9);
}

#[test]
fn walk_local_builds_workspace_cache_without_publishing() {
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
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript,
    )]);
    let scope = SpendScope::for_workspace(Some(&project), &[], None);
    let scope_hash = scope.hash();
    let previous = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: 10_000,
        scope_hash: scope_hash.clone(),
        live_baselines: BTreeMap::from([("claude:old".to_owned(), 1.0)]),
        ..Default::default()
    };
    let workspace_path = runtime.workspace_spending_path(&scope_hash);
    write_workspace_spending_cache(&workspace_path, &previous);
    let before_bytes = std::fs::read(&workspace_path).expect("workspace cache");
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let mut walker = SpendingWalker::new();

    let local = walk_fleet_spending(&mut walker, &runtime, Some(&project), &spec, false);

    assert!((local.workspace.tally.headline.usd - 2.5).abs() < 1e-9);
    assert_eq!(local.workspace.live_baselines.len(), 1);
    assert_eq!(
        std::fs::read(&workspace_path).expect("workspace cache"),
        before_bytes,
        "local fallback computes workspace cache for its own frame without publishing"
    );
}

#[test]
fn publishing_walk_observer_checkpoints_workspace_live_baselines() {
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
                stat: crate::agents::TranscriptStat {
                    mtime_secs: 1,
                    len: 1,
                    ..crate::agents::TranscriptStat::default()
                },
                cursor: SpendCursor::default(),
                origin_path: Some(project),
                entries: vec![CachedEntry {
                    ts_secs: now_secs,
                    cost_usd: 1.25,
                    input: 10,
                    output: 5,
                    cache_write: 0,
                    cache_read: 0,
                    tool_calls: Default::default(),
                    message_id: Some("msg-1".to_owned()),
                    request_id: Some("req-1".to_owned()),
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
        cache
    };
    let files = vec![(
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript,
    )];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let provider_path = runtime.shared_provider_spending_path();
    let user_inputs = Vec::new();
    let mut progress = |_| {};
    let mut observer = super::PublishingWalkObserver {
        runtime: &runtime,
        provider_path,
        files: &files,
        user_inputs: &user_inputs,
        now_secs,
        scope: Some(&scope),
        scope_hash: Some(scope_hash.clone()),
        spec: &spec,
        progress: &mut progress,
    };

    crate::agents::spending::WalkObserver::on_interval(&mut observer, &raw);

    let workspace = read_workspace_spending_cache(&runtime.workspace_spending_path(&scope_hash));
    assert!((workspace.tally.headline.usd - 1.25).abs() < 1e-9);
    assert_eq!(workspace.live_baselines.len(), 1);
    assert_eq!(
        workspace.live_baselines.values().copied().sum::<f64>(),
        1.25
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
            stat: crate::agents::TranscriptStat {
                mtime_secs: 1,
                len: 1,
                ..crate::agents::TranscriptStat::default()
            },
            cursor: SpendCursor::default(),
            origin_path: Some(project.clone()),
            entries: vec![CachedEntry {
                ts_secs: now_secs,
                cost_usd: 1.25,
                input: 10,
                output: 5,
                cache_write: 0,
                cache_read: 0,
                tool_calls: Default::default(),
                message_id: Some("msg-1".to_owned()),
                request_id: Some("req-1".to_owned()),
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
        crate::agents::definition_by_kind("claude").unwrap(),
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
        &mut SpendingWalker::new(),
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &HeadlineSpec {
            mode: SpendWindowMode::Today,
            timezone: Some("UTC".to_owned()),
        },
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
fn workspace_cache_from_shared_entries_publishes_live_exclusions() {
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
            stat: crate::agents::TranscriptStat {
                mtime_secs: 1,
                len: 1,
                ..crate::agents::TranscriptStat::default()
            },
            cursor: SpendCursor::default(),
            origin_path: Some(project.clone()),
            entries: vec![CachedEntry {
                ts_secs: now_secs,
                cost_usd: 1.25,
                input: 10,
                output: 5,
                cache_write: 0,
                cache_read: 0,
                tool_calls: Default::default(),
                message_id: Some("msg-1".to_owned()),
                request_id: Some("req-1".to_owned()),
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
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let files = vec![(
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript,
    )];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let scoped = crate::agents::spending::compute_scoped_spending(
        &files,
        &raw,
        &[],
        &scope,
        now_secs,
        &spec,
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
    let workspace = workspace_cache_from_shared_entries(
        &mut SpendingWalker::new(),
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &spec,
    )
    .expect("workspace cache derives from the shared cursor cache");

    assert!((workspace.tally.headline.usd - 1.25).abs() < 1e-9);
    assert_eq!(workspace.headline_cutoff_secs, scoped.headline_cutoff_secs);
    assert_eq!(workspace.live_baselines, scoped.live_baselines);
    let fresh = super::fresh_workspace_cache(&runtime, Some(&scope_hash), unix_now_ms())
        .expect("fresh workspace cache");
    assert_eq!(fresh, workspace);
}

#[test]
fn producer_missing_scope_applies_live_origin_before_reusing_warm_memo() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("repo");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&project), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scope_hash = scope.hash();
    let transcript = dir.path().join("claude.jsonl");
    std::fs::write(&transcript, b"").expect("transcript");
    let now_secs = unix_secs_now();
    let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        FileCacheEntry {
            stat: crate::agents::TranscriptStat::from_path(&transcript).unwrap(),
            cursor: SpendCursor::default(),
            origin_path: None,
            entries: vec![CachedEntry {
                ts_secs: now_secs,
                cost_usd: 1.25,
                input: 10,
                output: 5,
                cache_write: 0,
                cache_read: 0,
                tool_calls: Default::default(),
                message_id: Some("msg-1".to_owned()),
                request_id: Some("req-1".to_owned()),
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
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let files = vec![(
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript.clone(),
    )];
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };

    let provider = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: 1_000,
        ..Default::default()
    };
    let mut walker = SpendingWalker::new();
    let origin_overrides = HashMap::from([(transcript.clone(), project.clone())]);
    let included = super::workspace_cache_from_shared_entries_inner(
        &mut walker,
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &spec,
        &origin_overrides,
        true,
        now_secs,
    )
    .expect("producer derives the missing scope");

    assert!((included.tally.headline.usd - 1.25).abs() < 1e-9);
    assert_eq!(included.live_baselines.len(), 1);
    assert_eq!(included.live_baselines.values().copied().sum::<f64>(), 1.25);
    assert_eq!(
        read_spending_cache(&runtime.shared_spending_cursor_path())
            .files
            .get(&transcript.to_string_lossy().into_owned())
            .and_then(|entry| entry.origin_path.as_ref()),
        Some(&project)
    );

    let prices = crate::agents::pricing::PriceBook::default();
    let origin_overrides = std::collections::HashMap::new();
    let user_inputs = Vec::new();
    let request = crate::agents::spending::WalkRequest {
        files: &files,
        prices: &prices,
        now_secs: now_secs + 1,
        origin_overrides: &origin_overrides,
        user_inputs: &user_inputs,
        scope: Some(&scope),
        spec: &spec,
    };
    let warm = walker.walk_local(
        &runtime.shared_spending_cursor_path(),
        &request,
        &mut crate::agents::spending::SilentWalk,
    );
    assert!(!warm.stats.cache_parsed);
    assert_eq!(warm.stats.dedup_passes, 0);
}

#[test]
fn workspace_cache_from_shared_entries_serves_young_previous_regression() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("repo");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&project), dir.path())
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let scope_hash = scope.hash();
    let transcript = dir.path().join("claude.jsonl");
    std::fs::write(&transcript, b"").expect("transcript");
    let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        FileCacheEntry {
            stat: crate::agents::TranscriptStat {
                mtime_secs: 1,
                ..crate::agents::TranscriptStat::default()
            },
            cursor: SpendCursor::default(),
            origin_path: Some(project),
            entries: Vec::new(),
            unknown_models: BTreeMap::new(),
        },
    );
    write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let files = vec![(
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript,
    )];
    let mut spending = Spending::default();
    spending.total.year.usd = 10.0;
    let provider = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: unix_now_ms(),
        spending,
        ..ProviderSpendingCache::default()
    };
    let mut prev_tally = crate::agents::spending::SpendTally::default();
    prev_tally.headline.usd = 5.0;
    let spec = HeadlineSpec {
        mode: SpendWindowMode::Today,
        timezone: Some("UTC".to_owned()),
    };
    let day_cutoff_secs = Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .start_of_day()
        .expect("UTC day start")
        .timestamp()
        .as_second() as u64;
    let prev = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: unix_now_ms(),
        scope_hash: scope_hash.clone(),
        tally: prev_tally.clone(),
        headline_cutoff_secs: day_cutoff_secs,
        day_cutoff_secs,
        live_baselines: BTreeMap::from([("claude:old".to_owned(), 1.0)]),
        ..Default::default()
    };
    write_workspace_spending_cache(&runtime.workspace_spending_path(&scope_hash), &prev);

    let served = workspace_cache_from_shared_entries(
        &mut SpendingWalker::new(),
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &spec,
    )
    .expect("workspace cache derives from shared entries");

    assert_eq!(served.tally, prev_tally);
    assert_eq!(served.live_baselines, prev.live_baselines);

    let old = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: unix_now_ms()
            .saturating_sub(crate::agents::spending::SESSION_GAP_SECS * 1_000 + 1),
        ..prev
    };
    write_workspace_spending_cache(&runtime.workspace_spending_path(&scope_hash), &old);
    let reset = workspace_cache_from_shared_entries(
        &mut SpendingWalker::new(),
        &runtime,
        &provider,
        &scope,
        Some(&scope_hash),
        &files,
        &spec,
    )
    .expect("workspace cache derives from shared entries");

    assert_eq!(reset.tally.headline.usd, 0.0);
    assert!(reset.live_baselines.is_empty());
}

#[test]
fn workspace_regression_guard_never_carries_spend_across_local_midnight() {
    let now_ms = unix_now_ms();
    let prev = crate::agents::spending::WorkspaceSpendingCache {
        version: crate::agents::spending::WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms: now_ms,
        scope_hash: "scope".to_owned(),
        tally: crate::agents::spending::SpendTally {
            headline: crate::agents::spending::SpendWindow {
                usd: 5.0,
                ..Default::default()
            },
            ..Default::default()
        },
        day: crate::agents::spending::SpendWindow {
            usd: 20.0,
            ..Default::default()
        },
        day_cutoff_secs: 100,
        ..Default::default()
    };
    let next = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: now_ms + 1,
        day: crate::agents::spending::SpendWindow {
            usd: 0.25,
            ..Default::default()
        },
        day_cutoff_secs: 200,
        ..prev.clone()
    };

    assert_eq!(
        super::serve_prev_on_young_regression(prev, next.clone(), now_ms + 1),
        next
    );
}

#[test]
fn workspace_regression_guard_propagates_a_genuine_burst_end() {
    let now_ms = unix_now_ms();
    let prev = crate::agents::spending::WorkspaceSpendingCache {
        version: crate::agents::spending::WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms: now_ms,
        scope_hash: "scope".to_owned(),
        tally: crate::agents::spending::SpendTally {
            headline: crate::agents::spending::SpendWindow {
                usd: 5.0,
                ..Default::default()
            },
            ..Default::default()
        },
        headline_cutoff_secs: 100,
        day_cutoff_secs: 50,
        ..Default::default()
    };
    let next = crate::agents::spending::WorkspaceSpendingCache {
        refreshed_at_ms: now_ms + 1,
        tally: Default::default(),
        headline_cutoff_secs: crate::agents::spending::NO_BURST_CUTOFF,
        ..prev.clone()
    };

    assert_eq!(
        super::serve_prev_on_young_regression(prev, next.clone(), now_ms + 1),
        next
    );
}

#[test]
fn producer_publishes_compacted_shared_spending_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let transcript = dir.path().join("claude.jsonl");
    std::fs::write(&transcript, b"").expect("transcript");
    let now_secs = unix_secs_now();
    let old_a = CachedEntry {
        ts_secs: now_secs - 40 * 86_400,
        cost_usd: 1.0,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        tool_calls: Default::default(),
        message_id: None,
        request_id: None,
        dedup_key: None,
        thread_id: Some("old".to_owned()),
        is_sidechain: false,
        has_speed: false,
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
            stat: crate::agents::TranscriptStat::from_path(&transcript).unwrap(),
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
        crate::agents::definition_by_kind("claude").unwrap(),
        transcript,
    )]);
    let caches = compute_fleet_spending(&runtime, None, &HeadlineSpec::default());

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
    let cache = compute_fleet_spending(&runtime, Some(&project), &HeadlineSpec::default());

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
