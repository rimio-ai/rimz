use super::*;

#[test]
fn cache_hit_skips_io_and_version_gate_discards_old_entries() {
    assert_eq!(SPENDING_CACHE_VERSION, 20);
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&today, 0.5, "msg-1", "req-1")],
    );

    let mut cache = SpendingDiskCache::default();
    let first = compute_total(std::slice::from_ref(&file), &mut cache);
    assert_eq!(first.headline.tokens, 15);
    assert!(cache.dirty);

    cache.dirty = false;
    let second = compute_total(&[file], &mut cache);
    assert_eq!(second.headline.usd, first.headline.usd);
    assert!(!cache.dirty, "unchanged files should be served from cache");

    let path = dir.path().join("spending.json");
    let stale = SpendingDiskCache {
        version: 18,
        files: HashMap::from([(
            "/old/chat.jsonl".to_string(),
            FileCacheEntry {
                stat: crate::agents::TranscriptStat {
                    mtime_secs: 123,
                    ..crate::agents::TranscriptStat::default()
                },
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![CachedEntry {
                    ts_secs: NOW_SECS,
                    cost_usd: 9.0,
                    input: 0,
                    output: 0,
                    cache_write: 0,
                    cache_read: 0,
                    message_id: Some("msg-old".to_string()),
                    request_id: Some("req-old".to_string()),
                    dedup_key: None,
                    thread_id: None,
                    is_sidechain: false,
                    has_speed: false,
                    model: None,
                    rolled: false,
                }],
                unknown_models: BTreeMap::new(),
            },
        )]),
        ..Default::default()
    };
    write_spending_cache(&path, &stale);

    let healed = read_spending_cache(&path);
    assert_eq!(healed.version, SPENDING_CACHE_VERSION);
    assert!(healed.files.is_empty());
}

#[test]
fn transient_file_stat_failure_preserves_the_cached_parse() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&claude_line(&utc_date(NOW_SECS), 0.5, "msg-1", "req-1")],
    );
    let files = vec![(claude_adapter(), file.clone())];
    let cache_path = dir.path().join("spending.json");
    let mut walker = SpendingWalker::new();
    let first = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut SilentWalk
    );

    std::fs::rename(&file, dir.path().join("parked.jsonl")).unwrap();
    let retry = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut SilentWalk
    );
    assert_eq!(retry.spending, first.spending);
    assert_eq!(retry.stats.parse_jobs, 0);
}

#[test]
fn spending_walk_threads_user_inputs_into_session_headline() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "session.jsonl",
        &[&claude_line_ts(
            &iso_at(NOW_SECS),
            1.0,
            "msg-session",
            "req-session",
        )],
    );
    let files = [(claude_adapter(), file)];
    let prices = PriceBook::default();
    let origin_overrides = HashMap::new();
    let user_inputs = [user_input::UserInputRecord {
        at: jiff::Timestamp::from_second(NOW_SECS as i64).unwrap(),
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        origin: None,
    }];
    let spec = HeadlineSpec::default();
    let mut req = WalkRequest {
        files: &files,
        prices: &prices,
        now_secs: NOW_SECS,
        origin_overrides: &origin_overrides,
        user_inputs: &user_inputs,
        scope: None,
        spec: &spec,
    };
    let mut walker = SpendingWalker::new();
    let cache_path = dir.path().join("spending.json");

    let included = walker.walk_local(&cache_path, &req, &mut SilentWalk);
    req.user_inputs = &[];
    let empty = walker.walk_local(&cache_path, &req, &mut SilentWalk);

    assert!((included.spending.total.headline.usd - 1.0).abs() < 1e-9);
    assert_eq!(empty.spending.total.headline, SpendWindow::default());
    assert_eq!(empty.stats.dedup_passes, 0);
}

#[test]
fn spending_walker_retains_only_winner_locations() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let padding = "x".repeat(32 * 1024);
    let first = claude_line(&today, 1.0, "msg-a", "req-a").replace(
        "{\"timestamp\"",
        &format!("{{\"unused\":\"{padding}\",\"timestamp\""),
    );
    let second = claude_line(&today, 2.0, "msg-b", "req-b").replace(
        "{\"timestamp\"",
        &format!("{{\"unused\":\"{padding}\",\"timestamp\""),
    );
    let file = write_jsonl(dir.path(), "large.jsonl", &[&first, &second]);
    let files = vec![(claude_adapter(), file)];
    let cache_path = dir.path().join("spending.json");
    let mut walker = SpendingWalker::new();

    let cold = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut SilentWalk
    );
    let memo = walker.memo.as_ref().expect("dedup memo");
    assert_eq!(
        memo.counted.as_ref(),
        &[
            CountedLocation {
                file_index: 0,
                entry_index: 0,
            },
            CountedLocation {
                file_index: 0,
                entry_index: 1,
            },
        ]
    );
    assert_eq!(
        std::mem::size_of_val(memo.counted.as_ref()),
        2 * std::mem::size_of::<CountedLocation>()
    );

    let warm = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS + 1,
        &mut SilentWalk
    );
    assert_eq!(warm.spending, cold.spending);
    assert_eq!(warm.stats.dedup_passes, 0);
}

#[test]
fn file_change_cache_paths_parse_suffix_or_reparse_cold() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);

    let suffix_file = write_jsonl(
        dir.path(),
        "suffix.jsonl",
        &[&claude_line(&today, 1.0, "msg-1", "req-1")],
    );
    let mut cache = SpendingDiskCache::default();
    let first = compute_spending(
        &[(claude_adapter(), suffix_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((first.total.headline.usd - 1.0).abs() < 1e-9);
    let prefix_len = std::fs::metadata(&suffix_file).unwrap().len() as usize;
    {
        use std::io::{Seek as _, SeekFrom};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&suffix_file)
            .unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![b'x'; prefix_len - 1]).unwrap();
    }
    append_line(&suffix_file, &claude_line(&today, 0.25, "msg-2", "req-2"));
    let suffix = compute_spending(
        &[(claude_adapter(), suffix_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((suffix.total.headline.usd - 1.25).abs() < 1e-9);

    let line_a = claude_line(&today, 1.0, "msg-a", "req-a");
    let line_b = claude_line(&today, 0.5, "msg-b", "req-b");
    let truncated_file = write_jsonl(dir.path(), "truncated.jsonl", &[&line_a, &line_b]);
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), truncated_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    write_jsonl(dir.path(), "truncated.jsonl", &[&line_a]);
    let truncated = compute_spending(
        &[(claude_adapter(), truncated_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((truncated.total.headline.usd - 1.0).abs() < 1e-9);

    let rewrite_file = write_jsonl(
        dir.path(),
        "rewrite.jsonl",
        &[&claude_line(&today, 1.0, "msg-r", "req-r")],
    );
    set_file_mtime_nanos(&rewrite_file, NOW_SECS, 100);
    let mut cache = SpendingDiskCache::default();
    compute_spending(
        &[(claude_adapter(), rewrite_file.clone())],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    let warmed_stat = cache.files[&rewrite_file.to_string_lossy().into_owned()].stat;
    write_jsonl(
        dir.path(),
        "rewrite.jsonl",
        &[&claude_line(&today, 3.0, "msg-r", "req-r")],
    );
    set_file_mtime_nanos(&rewrite_file, NOW_SECS, 200);
    let rewritten_stat = crate::agents::TranscriptStat::from_path(&rewrite_file).unwrap();
    assert_eq!(warmed_stat.mtime_secs, rewritten_stat.mtime_secs);
    assert_eq!(warmed_stat.mtime_nanos, 100);
    assert_eq!(rewritten_stat.mtime_secs, i64::try_from(NOW_SECS).unwrap());
    assert_eq!(rewritten_stat.mtime_nanos, 200);
    let rewritten = compute_spending(
        &[(claude_adapter(), rewrite_file)],
        &mut cache,
        &PriceBook::default(),
        NOW_SECS,
    );
    assert!((rewritten.total.headline.usd - 3.0).abs() < 1e-9);
}

#[test]
fn opencode_wal_commit_refreshes_spending() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opencode.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA wal_autocheckpoint = 0;\
             CREATE TABLE message (id TEXT, session_id TEXT, data TEXT);",
        )
        .unwrap();
    let initial = r#"{"cost":1.25,"modelID":"gpt","providerID":"openai","time":{"created":1750000000000},"tokens":{"input":10,"output":5}}"#;
    let updated = r#"{"cost":9.75,"modelID":"gpt","providerID":"openai","time":{"created":1750000000000},"tokens":{"input":10,"output":5}}"#;
    assert_eq!(initial.len(), updated.len());
    connection
        .execute(
            "INSERT INTO message (id, session_id, data) VALUES ('msg', 'ses', ?1)",
            [initial],
        )
        .unwrap();

    let files = [(opencode_adapter(), path.clone())];
    let mut cache = SpendingDiskCache::default();
    let warmed = compute_spending(&files, &mut cache, &PriceBook::default(), NOW_SECS);
    assert!((warmed.total.headline.usd - 1.25).abs() < 1e-9);
    let main_before = crate::agents::TranscriptStat::from_path(&path).unwrap();
    let logical_before = opencode_adapter().transcript_stat(&path).unwrap();
    assert!(logical_before.companion.is_some());

    connection
        .execute("UPDATE message SET data = ?1 WHERE id = 'msg'", [updated])
        .unwrap();

    let main_after = crate::agents::TranscriptStat::from_path(&path).unwrap();
    let logical_after = opencode_adapter().transcript_stat(&path).unwrap();
    assert_eq!(main_after, main_before, "the commit stayed in the held WAL");
    assert_ne!(
        logical_after.companion, logical_before.companion,
        "the WAL identity records the equal-length commit"
    );

    let refreshed = compute_spending(&files, &mut cache, &PriceBook::default(), NOW_SECS);
    assert!((refreshed.total.headline.usd - 9.75).abs() < 1e-9);
    assert_eq!(
        cache.files[&path.to_string_lossy().into_owned()]
            .entries
            .len(),
        1,
        "the authoritative table fold replaces rather than double-counts"
    );
}

#[test]
fn cold_parse_skip_ignores_new_files_outside_widest_window() {
    assert!(!cold_parse_out_of_window(
        1_000,
        1_000 + WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS
    ));
    assert!(cold_parse_out_of_window(
        1_000,
        1_001 + WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS
    ));

    let dir = TempDir::new().unwrap();
    let file = write_jsonl(dir.path(), "old.jsonl", &[]);
    let mtime = crate::agents::TranscriptStat::from_path(&file)
        .unwrap()
        .newest_mtime_secs();
    std::fs::write(
        &file,
        format!("{}\n", claude_line_ts(&iso_at(mtime), 1.0, "old", "old")),
    )
    .unwrap();
    let mtime = crate::agents::TranscriptStat::from_path(&file)
        .unwrap()
        .newest_mtime_secs();
    let files = vec![(claude_adapter(), file.clone())];
    let mut cache = SpendingDiskCache::default();

    let skipped = compute_spending(
        &files,
        &mut cache,
        &PriceBook::default(),
        mtime + WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS + 1,
    );

    assert_eq!(skipped.total.year.usd, 0.0);
    assert!(
        !cache
            .files
            .contains_key(&file.to_string_lossy().into_owned())
    );
    assert!(!cache.dirty);

    let fresh_file = write_jsonl(
        dir.path(),
        "fresh.jsonl",
        &[&claude_line_ts(&iso_at(mtime), 1.0, "fresh", "fresh")],
    );
    let mut fresh_cache = SpendingDiskCache::default();
    let parsed = compute_spending(
        &[(claude_adapter(), fresh_file)],
        &mut fresh_cache,
        &PriceBook::default(),
        mtime + 60,
    );

    assert!((parsed.total.year.usd - 1.0).abs() < 1e-9);
}

#[test]
fn unknown_model_chase_filters_sentinels_and_stale_records() {
    assert!(!is_priceable_model_name("<synthetic>"));
    assert!(!is_priceable_model_name("   "));
    assert!(is_priceable_model_name("claude-new"));

    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let model = "new-claude-pricing-test-model";
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[&format!(
            r#"{{"timestamp":"{today}T15:00:00.000Z","requestId":"req-1","message":{{"id":"msg-1","model":"{model}","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
        )],
    );
    let mut cache = SpendingDiskCache::default();
    let spending = compute_spending(
        &[(claude_adapter(), file.clone())],
        &mut cache,
        &PriceBook::from_litellm_json("{}"),
        NOW_SECS,
    );
    assert_eq!(spending.total.headline.tokens, 150);
    assert_eq!(
        recorded_unknown_models(&[(claude_adapter(), file.clone())], &cache, NOW_SECS),
        std::collections::BTreeSet::from([model.to_owned()])
    );

    cache
        .files
        .get_mut(&file.to_string_lossy().into_owned())
        .unwrap()
        .unknown_models
        .insert(
            "stale-model".to_owned(),
            NOW_SECS.saturating_sub(WIDEST_SPEND_WINDOW_SECS),
        );
    assert_eq!(
        recorded_unknown_models(&[(claude_adapter(), file)], &cache, NOW_SECS),
        std::collections::BTreeSet::from([model.to_owned()])
    );
}

#[test]
fn spending_walk_observer_checkpoints_on_first_interval() {
    struct CaptureObserver {
        cache_path: PathBuf,
        intervals: usize,
        saw_cursor_file: bool,
    }

    impl WalkObserver for CaptureObserver {
        fn on_interval(&mut self, _cache: &SpendingDiskCache) {
            self.intervals += 1;
            self.saw_cursor_file |= self.cache_path.exists();
        }
    }

    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let files = (0..4)
        .map(|i| {
            let file = write_jsonl(
                dir.path(),
                &format!("cold-{i}.jsonl"),
                &[&claude_line(
                    &today,
                    1.0,
                    &format!("msg-{i}"),
                    &format!("req-{i}"),
                )],
            );
            (claude_adapter(), file)
        })
        .collect::<Vec<_>>();
    let cache_path = dir.path().join("spending.json");
    let mut observer = CaptureObserver {
        cache_path: cache_path.clone(),
        intervals: 0,
        saw_cursor_file: false,
    };
    let mut walker = SpendingWalker::new();

    let result = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut observer
    );

    assert!(observer.intervals >= 1);
    assert!(observer.saw_cursor_file);
    assert!((result.spending.total.year.usd - 4.0).abs() < 1e-9);
}

#[test]
fn parallel_cold_parse_aggregates_deterministically() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let files = (0..16)
        .map(|i| {
            let cost = (i + 1) as f64 / 10.0;
            let file = write_jsonl(
                dir.path(),
                &format!("parallel-{i}.jsonl"),
                &[&claude_line(
                    &today,
                    cost,
                    &format!("msg-parallel-{i}"),
                    &format!("req-parallel-{i}"),
                )],
            );
            (claude_adapter(), file)
        })
        .collect::<Vec<_>>();
    let expected = (1..=16).map(|i| i as f64 / 10.0).sum::<f64>();

    let mut first_walker = SpendingWalker::new();
    let mut second_walker = SpendingWalker::new();
    let first = walk_spending!(
        first_walker,
        &dir.path().join("first-spending.json"),
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut SilentWalk
    );
    let second = walk_spending!(
        second_walker,
        &dir.path().join("second-spending.json"),
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut SilentWalk
    );

    assert!((first.spending.total.year.usd - expected).abs() < 1e-9);
    assert_eq!(first.spending, second.spending);
    assert_eq!(
        serde_json::to_vec(&first.spending).unwrap(),
        serde_json::to_vec(&second.spending).unwrap()
    );
}

#[test]
fn spending_walk_persists_cursor_before_aggregate() {
    let dir = TempDir::new().unwrap();
    let cache_path = dir.path().join("spending.json");
    let transcript = write_jsonl(dir.path(), "claude.jsonl", &[]);
    let mut old_a = cached_entry(NOW_SECS - 40 * 86_400, 1.0, "old");
    old_a.model = Some("claude-opus-4-8".to_owned());
    let mut old_b = cached_entry(NOW_SECS - 40 * 86_400, 2.0, "old");
    old_b.model = Some("claude-opus-4-8".to_owned());
    let mut cache = read_spending_cache(&cache_path);
    cache.files = HashMap::from([cached_file(&transcript, vec![old_a, old_b])]);
    write_spending_cache(&cache_path, &cache);
    let files: Vec<(&'static AgentDefinition, PathBuf)> =
        vec![(claude_adapter(), transcript.clone())];
    let mut walker = SpendingWalker::new();

    panic_after_next_refresh_for_test();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut observer = SilentWalk;
        walk_spending!(
            walker,
            &cache_path,
            &files,
            PriceBook::default(),
            NOW_SECS,
            &mut observer
        )
    }));

    assert!(
        result.is_err(),
        "test panic fires after refresh, before aggregation"
    );
    let persisted = read_spending_cache(&cache_path);
    let entries = &persisted.files[&transcript.to_string_lossy().into_owned()].entries;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].rolled);
    assert_eq!(entries[0].cost_usd, 3.0);
}

#[test]
fn spending_walk_gates_warm_cursor_persists() {
    let dir = TempDir::new().unwrap();
    let today = utc_date(NOW_SECS);
    let cache_path = dir.path().join("spending.json");
    let transcript = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line(&today, 1.0, "msg-1", "req-1")],
    );
    set_file_mtime(&transcript, NOW_SECS);
    let files: Vec<(&'static AgentDefinition, PathBuf)> =
        vec![(claude_adapter(), transcript.clone())];
    let mut walker = SpendingWalker::new();

    let first = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS,
        &mut SilentWalk
    );
    assert!(first.stats.cache_written);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        1
    );
    let first_stamp = cache_stamp(&cache_path);

    append_line(&transcript, &claude_line(&today, 2.0, "msg-2", "req-2"));
    set_file_mtime(&transcript, NOW_SECS + 60);
    let second = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS + 60,
        &mut SilentWalk
    );
    assert!(!second.stats.cache_written);
    assert!((second.spending.total.year.usd - 3.0).abs() < 1e-9);
    assert_eq!(cache_stamp(&cache_path), first_stamp);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        1,
        "warm suffix stays in memory until the persist interval expires"
    );

    let third = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL + 61,
        &mut SilentWalk
    );
    assert!(third.stats.cache_written);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        2,
        "post-interval walk lands previously dirty in-memory state"
    );

    let padding = "x".repeat(SPENDING_PERSIST_PARSE_BYTES as usize);
    append_line(
        &transcript,
        &format!(
            r#"{{"timestamp":"{today}T15:02:00.000Z","costUSD":3.0,"requestId":"req-big","padding":"{padding}","message":{{"id":"msg-big","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        ),
    );
    set_file_mtime(&transcript, NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL + 62);
    let fourth = walk_spending!(
        walker,
        &cache_path,
        &files,
        PriceBook::default(),
        NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL + 62,
        &mut SilentWalk
    );
    assert!(fourth.stats.parse_bytes >= SPENDING_PERSIST_PARSE_BYTES);
    assert!(fourth.stats.cache_written);
    assert!((fourth.spending.total.year.usd - 6.0).abs() < 1e-9);
    assert_eq!(
        read_spending_cache(&cache_path).files[&transcript.to_string_lossy().into_owned()]
            .entries
            .len(),
        3,
        "large parse work persists regardless of the interval"
    );
}

#[test]
fn live_origin_updates_share_the_walk_persist_gate() {
    let dir = TempDir::new().unwrap();
    let cache_path = dir.path().join("spending.json");
    let transcript = dir.path().join("chat.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    let key = transcript.to_string_lossy().into_owned();
    let mut cache = read_spending_cache(&cache_path);
    cache.files.insert(
        key.clone(),
        FileCacheEntry {
            stat: crate::agents::TranscriptStat::default(),
            cursor: SpendCursor::default(),
            origin_path: None,
            entries: Vec::new(),
            unknown_models: BTreeMap::new(),
        },
    );
    assert!(write_spending_cache(&cache_path, &cache));
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    let mut walker = SpendingWalker::new();

    walker.apply_origin_overrides(
        &cache_path,
        &HashMap::from([(transcript.clone(), first.clone())]),
        true,
        NOW_SECS,
    );
    assert_eq!(
        read_spending_cache(&cache_path).files[&key]
            .origin_path
            .as_ref(),
        Some(&first)
    );

    walker.apply_origin_overrides(
        &cache_path,
        &HashMap::from([(transcript.clone(), second.clone())]),
        true,
        NOW_SECS + 1,
    );
    assert_eq!(
        read_spending_cache(&cache_path).files[&key]
            .origin_path
            .as_ref(),
        Some(&first),
        "a second live origin stays in memory inside the persist interval"
    );

    walker.apply_origin_overrides(
        &cache_path,
        &HashMap::from([(transcript, second.clone())]),
        true,
        NOW_SECS + SPENDING_PERSIST_MIN_INTERVAL,
    );
    assert_eq!(
        read_spending_cache(&cache_path).files[&key]
            .origin_path
            .as_ref(),
        Some(&second)
    );
}
