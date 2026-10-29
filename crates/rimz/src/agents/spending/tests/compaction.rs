use super::*;

#[test]
fn cache_compaction_releases_spare_capacity() {
    let file = PathBuf::from("/x/compacted.jsonl");
    let mut entries = vec![cached_entry(NOW_SECS - 40 * 86_400, 1.0, "old"); 1000];
    entries.push(cached_entry(NOW_SECS, 1.0, "recent"));
    let mut absent_entries = Vec::with_capacity(1000);
    absent_entries.push(cached_entry(NOW_SECS, 1.0, "absent"));
    let mut cache = SpendingDiskCache {
        files: HashMap::with_capacity(1000),
        ..Default::default()
    };
    cache.files.extend([
        cached_file(&file, entries),
        cached_file(Path::new("/x/absent.jsonl"), absent_entries),
    ]);
    assert!(compact_spending_cache(
        &mut cache,
        &[spending_file(claude_adapter(), file)],
        NOW_SECS,
    ));
    assert!(cache.files["/x/compacted.jsonl"].entries.len() < 10);
    let actual = (
        cache
            .files
            .values()
            .map(|file| file.entries.capacity())
            .sum::<usize>(),
        cache.files.capacity(),
    );
    let expected = (
        cache
            .files
            .values()
            .map(|file| file.entries.len())
            .sum::<usize>(),
        HashMap::<String, FileCacheEntry>::with_capacity(cache.files.len()).capacity(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn cache_compaction_rolls_old_entries_losslessly_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let other = dir.path().join("other");
    let project_file = dir.path().join("claude.jsonl");
    let other_file = dir.path().join("other.jsonl");
    let mut old_project = cached_entry(NOW_SECS - 40 * 86_400, 1.0, "old-project");
    old_project.model = Some("claude-opus-4-8".into());
    old_project.timing = Some(ResponseTiming {
        ms: 2_000,
        output: 100,
    });
    let mut old_project_same_bucket = cached_entry(NOW_SECS - 40 * 86_400, 2.0, "old-project");
    old_project_same_bucket.model = Some("claude-opus-4-8".into());
    old_project_same_bucket.fast = true;
    old_project_same_bucket.timing = Some(ResponseTiming {
        ms: 4_000,
        output: 300,
    });
    let mut old_other = cached_entry(NOW_SECS - 60 * 86_400, 3.0, "old-other");
    old_other.model = Some("claude-sonnet-4-6".into());
    old_other.fast = true;
    let mut recent = cached_entry(NOW_SECS - RAW_RETAIN_SECS + 86_400, 4.0, "recent");
    recent.model = Some("claude-opus-4-8".into());
    let expired_rollup_ts = NOW_SECS - 400 * 86_400;
    let mut expired_rollup = cached_entry(expired_rollup_ts, 9.0, "expired");
    expired_rollup.model = Some("claude-opus-4-8".into());
    expired_rollup.rolled = true;
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file_with_origin(
                &project_file,
                &project,
                vec![
                    old_project,
                    old_project_same_bucket,
                    recent.clone(),
                    expired_rollup,
                ],
            ),
            cached_file_with_origin(&other_file, &other, vec![old_other]),
        ]),
        ..Default::default()
    };
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), project_file.clone()),
        spending_file(claude_adapter(), other_file.clone()),
    ];
    let scope = SpendScope::from_roots(Some(&project), &[]);
    let before_counted = dedup_cached_entries(&files, &cache).into_counted();
    let before_spending = aggregate_spending(
        &files,
        &cache,
        &before_counted,
        NOW_SECS,
        &HeadlineSpec::default(),
    );
    let mut expected_cache = cache.clone();
    expected_cache
        .files
        .get_mut(&project_file.to_string_lossy().into_owned())
        .unwrap()
        .entries
        .retain(|entry| entry.ts_secs != expired_rollup_ts);
    let before_days = compute_daily_spend(&files, &expected_cache);
    let before_models = compute_model_breakdown(&files, &cache, NOW_SECS);
    let before_scoped =
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default());

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));

    let rolled = cache
        .files
        .values()
        .flat_map(|file| &file.entries)
        .filter(|entry| entry.rolled)
        .collect::<Vec<_>>();
    assert_eq!(rolled.len(), 2);
    assert!(rolled.iter().all(|entry| entry.fast));

    let after_counted = dedup_cached_entries(&files, &cache).into_counted();
    assert_eq!(
        aggregate_spending(
            &files,
            &cache,
            &after_counted,
            NOW_SECS,
            &HeadlineSpec::default()
        ),
        before_spending
    );
    assert_eq!(compute_daily_spend(&files, &cache), before_days);
    assert_eq!(
        compute_model_breakdown(&files, &cache, NOW_SECS),
        before_models
    );
    assert_eq!(
        compute_scoped_tally(&files, &cache, &scope, NOW_SECS, &HeadlineSpec::default()),
        before_scoped
    );
    let entries = &cache.files[&project_file.to_string_lossy().into_owned()].entries;
    assert_eq!(
        entries.iter().find(|entry| entry.rolled).unwrap().timing,
        Some(ResponseTiming {
            ms: 6_000,
            output: 400
        })
    );
    assert!(
        entries
            .iter()
            .any(|entry| !entry.rolled && entry.ts_secs == recent.ts_secs)
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.ts_secs != expired_rollup_ts)
    );
    assert_eq!(
        cache
            .files
            .values()
            .flat_map(|file| &file.entries)
            .filter(|entry| entry.rolled)
            .count(),
        2
    );

    let encoded = serde_json::to_value(&cache.files).unwrap();
    assert!(!compact_spending_cache(&mut cache, &files, NOW_SECS));
    assert_eq!(serde_json::to_value(&cache.files).unwrap(), encoded);
}

#[test]
fn cache_compaction_handles_sidechain_replay_edges() {
    let project = PathBuf::from("/projects/slug");
    let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let main = project.join(format!("{id}.jsonl"));
    let replay = project.join(id).join("subagents/agent-worker.jsonl");
    let ts = NOW_SECS - 40 * 86_400;
    let main_entry = CachedEntry {
        ts_secs: ts,
        cost_usd: 1.0,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        tool_calls: Default::default(),
        message_id: Some("msg-1".into()),
        request_id: Some("req-1".into()),
        dedup_key: None,
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        fast: false,
        model: Some("claude-opus-4-8".into()),
        rolled: false,
        timing: None,
    };
    let replay_entry = CachedEntry {
        cost_usd: 9.0,
        input: 9_000,
        is_sidechain: true,
        ..main_entry.clone()
    };
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            cached_file(&main, vec![main_entry]),
            cached_file(&replay, vec![replay_entry]),
        ]),
        ..Default::default()
    };
    let files: Vec<SpendingFile> = vec![
        spending_file(claude_adapter(), main.clone()),
        spending_file(claude_adapter(), replay.clone()),
    ];

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.year.usd, 1.0);
    assert_eq!(spending.total.year.tokens, 15);
    assert_eq!(
        cache
            .files
            .values()
            .flat_map(|file| &file.entries)
            .filter(|entry| entry.rolled)
            .count(),
        1
    );

    let file = PathBuf::from("/x/claude.jsonl");
    let old_main = CachedEntry {
        ts_secs: NOW_SECS - 40 * 86_400,
        cost_usd: 1.0,
        input: 10,
        output: 5,
        cache_write: 0,
        cache_read: 0,
        tool_calls: Default::default(),
        message_id: Some("msg-1".into()),
        request_id: Some("req-1".into()),
        dedup_key: None,
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        fast: false,
        model: Some("claude-opus-4-8".into()),
        rolled: false,
        timing: None,
    };
    let recent_replay = CachedEntry {
        ts_secs: NOW_SECS - RAW_RETAIN_SECS + 86_400,
        cost_usd: 9.0,
        input: 9_000,
        is_sidechain: true,
        ..old_main.clone()
    };
    let mut cache = SpendingDiskCache {
        files: HashMap::from([cached_file(&file, vec![old_main, recent_replay])]),
        ..Default::default()
    };
    let files: Vec<SpendingFile> = vec![spending_file(claude_adapter(), file.clone())];

    assert!(!compact_spending_cache(&mut cache, &files, NOW_SECS));
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.year.usd, 1.0);
    assert!(
        cache.files[&file.to_string_lossy().into_owned()]
            .entries
            .iter()
            .all(|entry| !entry.rolled)
    );
}

#[test]
fn cache_compaction_evicts_dead_file_records() {
    let dir = TempDir::new().unwrap();
    let old_mtime = NOW_SECS - WIDEST_SPEND_WINDOW_SECS - SKIP_PARSE_MARGIN_SECS - 10;
    let recent_mtime = NOW_SECS - 60;
    let discovered_old = write_jsonl(dir.path(), "old.jsonl", &[]);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&discovered_old)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(old_mtime))
        .unwrap();
    let discovered_stat = crate::agents::TranscriptStat::from_path(&discovered_old).unwrap();
    let discovered_len = discovered_stat.len;
    let deleted_old = dir.path().join("deleted.jsonl");
    let recent_absent = dir.path().join("recent.jsonl");
    let discovered_key = discovered_old.to_string_lossy().into_owned();
    let deleted_key = deleted_old.to_string_lossy().into_owned();
    let recent_key = recent_absent.to_string_lossy().into_owned();
    let mut cache = SpendingDiskCache {
        files: HashMap::from([
            (
                discovered_key.clone(),
                FileCacheEntry {
                    stat: discovered_stat,
                    cursor: SpendCursor::new(discovered_len),
                    origin_path: None,
                    entries: Vec::new(),
                    unknown_models: BTreeMap::new(),
                },
            ),
            (
                deleted_key.clone(),
                FileCacheEntry {
                    stat: crate::agents::TranscriptStat {
                        mtime_secs: i64::try_from(old_mtime).unwrap(),
                        ..crate::agents::TranscriptStat::default()
                    },
                    cursor: SpendCursor::default(),
                    origin_path: None,
                    entries: vec![cached_entry(old_mtime, 1.0, "deleted-old")],
                    unknown_models: BTreeMap::new(),
                },
            ),
            (
                recent_key.clone(),
                FileCacheEntry {
                    stat: crate::agents::TranscriptStat {
                        mtime_secs: i64::try_from(recent_mtime).unwrap(),
                        ..crate::agents::TranscriptStat::default()
                    },
                    cursor: SpendCursor::default(),
                    origin_path: None,
                    entries: vec![cached_entry(recent_mtime, 2.0, "recent-absent")],
                    unknown_models: BTreeMap::new(),
                },
            ),
        ]),
        ..Default::default()
    };

    compute_spending(
        &[spending_file(claude_adapter(), discovered_old)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );

    assert!(cache.dirty);
    assert!(!cache.files.contains_key(&discovered_key));
    assert!(!cache.files.contains_key(&deleted_key));
    assert!(cache.files.contains_key(&recent_key));
}

#[test]
fn cache_compaction_keeps_rows_two_sessions_share_raw() {
    let project = Path::new("/repo/project");
    let source = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let fork = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let source_path = PathBuf::from(format!("/projects/slug/{source}.jsonl"));
    let fork_path = PathBuf::from(format!("/projects/slug/{fork}.jsonl"));
    let mut shared = cached_entry(NOW_SECS - 10 * 86_400, 9.0, "");
    shared.thread_id = None;
    shared.message_id = Some("msg-shared".into());
    shared.request_id = Some("req-shared".into());
    let own = CachedEntry {
        ts_secs: NOW_SECS,
        cost_usd: 1.0,
        message_id: Some("msg-own".into()),
        request_id: Some("req-own".into()),
        ..shared.clone()
    };
    let scope = SpendScope::from_roots(Some(project), &[]);
    for source_first in [true, false] {
        let mut cache = SpendingDiskCache {
            version: SPENDING_CACHE_VERSION,
            files: HashMap::from([
                cached_file_with_origin(&source_path, project, vec![shared.clone()]),
                cached_file_with_origin(&fork_path, project, vec![shared.clone(), own.clone()]),
            ]),
            ..Default::default()
        };
        let mut files = vec![
            spending_file(claude_adapter(), source_path.clone()),
            spending_file(claude_adapter(), fork_path.clone()),
        ];
        if !source_first {
            files.reverse();
        }

        compact_spending_cache(&mut cache, &files, NOW_SECS);
        let scoped = compute_scoped_spending_checked(
            &files,
            &cache,
            &[],
            &scope,
            NOW_SECS,
            &HeadlineSpec::default(),
        );

        assert!(
            cache
                .files
                .values()
                .flat_map(|file| &file.entries)
                .all(|entry| !entry.rolled)
        );
        assert_eq!(
            scoped.live_baselines,
            BTreeMap::from([(format!("claude:{fork}"), 10.0)])
        );
        assert_eq!(scoped.tally.year.usd, 10.0);
    }
}

#[test]
fn cache_compaction_preserves_old_native_thread_sessions() {
    let file = PathBuf::from("/x/opencode.db");
    let mut cache = SpendingDiskCache {
        files: HashMap::from([cached_file(
            &file,
            vec![
                cached_entry(NOW_SECS - 40 * 86_400, 1.0, "session-a"),
                cached_entry(NOW_SECS - 40 * 86_400 + 60, 2.0, "session-b"),
            ],
        )]),
        ..Default::default()
    };
    let files: Vec<SpendingFile> = vec![spending_file(opencode_adapter(), file)];

    assert!(compact_spending_cache(&mut cache, &files, NOW_SECS));
    let counted = dedup_cached_entries(&files, &cache).into_counted();
    let spending = aggregate_spending(&files, &cache, &counted, NOW_SECS, &HeadlineSpec::default());

    assert_eq!(spending.total.year.sessions, 2);
    assert_eq!(spending.by_provider["opencode"].year.sessions, 2);
}
