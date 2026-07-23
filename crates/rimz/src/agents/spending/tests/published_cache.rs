use super::*;

#[test]
fn provider_cache_staleness_and_error_cases_are_explicit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("provider-spending.json");
    let spending = sample_spending();

    write_provider_spending_cache(&path, 12_345, &spending);
    let cache = read_provider_spending_cache(&path);
    assert_eq!(cache.version, PROVIDER_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 12_345);
    assert_eq!(cache.spending, spending);
    assert!(cache.days.is_empty());
    assert!(cache.models.is_empty());
    assert!(cache.is_fresh(12_345));

    let days = BTreeMap::from([(
        NOW_SECS as i64 / 86_400,
        DaySpend {
            usd: 1.25,
            tokens: 4_200,
        },
    )]);
    let models = BTreeMap::from([(
        "claude-opus-4-8".to_owned(),
        model_tally(4_200, 1.25, 3_000, 1_200, 0),
    )]);
    let local_day = BTreeMap::from([(
        "claude".to_owned(),
        SpendWindow {
            usd: 2.50,
            sessions: 1,
            ..Default::default()
        },
    )]);
    write_provider_spending_cache_with_day(
        &path, 12_346, &spending, &days, &models, &local_day, 12_000,
    );
    let cache = read_provider_spending_cache(&path);
    assert_eq!(cache.version, PROVIDER_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 12_346);
    assert_eq!(cache.spending, spending);
    assert_eq!(cache.days, days);
    assert_eq!(cache.models, models);
    assert_eq!(cache.day_by_provider, local_day);
    assert_eq!(cache.day_cutoff_secs, 12_000);

    std::fs::write(&path, serde_json::to_vec(&spending).unwrap()).unwrap();
    let pre_stamp = read_provider_spending_cache(&path);
    assert_eq!(pre_stamp.refreshed_at_ms, 0);
    assert_eq!(pre_stamp.spending, spending);
    assert!(pre_stamp.days.is_empty());
    assert!(pre_stamp.models.is_empty());
    assert!(!pre_stamp.is_fresh(NOW_SECS * 1_000));

    let now_ms = NOW_SECS * 1_000;
    let stale_shape = ProviderSpendingCache {
        version: 0,
        refreshed_at_ms: now_ms,
        spending: spending.clone(),
        ..ProviderSpendingCache::default()
    };
    std::fs::write(&path, serde_json::to_vec(&stale_shape).unwrap()).unwrap();
    let version_mismatch = read_provider_spending_cache(&path);
    assert_eq!(version_mismatch.spending, spending);
    assert!(!version_mismatch.is_fresh(now_ms));

    let v7_shape = ProviderSpendingCache {
        version: 7,
        refreshed_at_ms: now_ms,
        spending: spending.clone(),
        ..ProviderSpendingCache::default()
    };
    std::fs::write(&path, serde_json::to_vec(&v7_shape).unwrap()).unwrap();
    let stale_v7 = read_provider_spending_cache(&path);
    assert_eq!(stale_v7.spending, spending);
    assert!(!stale_v7.is_current_version());

    std::fs::write(&path, b"not json").unwrap();
    let corrupt = read_provider_spending_cache(&path);
    assert_eq!(corrupt.refreshed_at_ms, 0);
    assert_eq!(corrupt.spending, Spending::default());

    let ttl_ms = SPENDING_TTL.as_millis() as u64;
    let fresh = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms: 1_000,
        ..ProviderSpendingCache::default()
    };
    assert!(fresh.is_fresh(1_000 + ttl_ms));
    assert!(!fresh.is_fresh(1_001 + ttl_ms));
    assert!(fresh.is_fresh(500));
}

#[test]
fn workspace_spending_cache_is_scope_keyed_and_ttl_gated() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("workspace-spending.json");
    let mut tally = SpendTally::default();
    tally.headline.usd = 3.21;
    tally.headline.tokens = 1234;

    let written = WorkspaceSpendingCache {
        refreshed_at_ms: 10_000,
        scope_hash: "scope-a".to_owned(),
        tally: tally.clone(),
        headline_cutoff_secs: NO_BURST_CUTOFF,
        live_baselines: BTreeMap::from([("claude:session-1".to_owned(), 2.34)]),
        ..Default::default()
    };
    write_workspace_spending_cache(&path, &written);
    let cache = read_workspace_spending_cache(&path);

    assert_eq!(cache.version, WORKSPACE_SPENDING_VERSION);
    assert_eq!(cache.refreshed_at_ms, 10_000);
    assert_eq!(cache.scope_hash, "scope-a");
    assert_eq!(cache.tally, tally);
    assert_eq!(cache.headline_cutoff_secs, NO_BURST_CUTOFF);
    assert_eq!(
        cache.live_baselines,
        BTreeMap::from([("claude:session-1".to_owned(), 2.34)])
    );
    assert!(cache.is_fresh(10_000, "scope-a"));
    assert!(!cache.is_fresh(10_000, "scope-b"));
    assert!(!cache.is_fresh(10_001 + SPENDING_TTL.as_millis() as u64, "scope-a"));

    std::fs::write(&path, b"not json").unwrap();
    assert_eq!(
        read_workspace_spending_cache(&path),
        WorkspaceSpendingCache::default()
    );
}

#[test]
fn cache_version_prefix_gates_reads_and_writes() {
    fn write_cursor(path: &Path) {
        let cursor = SpendingDiskCache {
            version: SPENDING_CACHE_VERSION,
            ..SpendingDiskCache::default()
        };
        assert!(write_spending_cache(path, &cursor));
    }

    fn write_provider(path: &Path) {
        write_provider_spending_cache(path, 12_345, &sample_spending());
    }

    fn write_workspace(path: &Path) {
        write_workspace_spending_cache(
            path,
            &WorkspaceSpendingCache {
                refreshed_at_ms: 12_345,
                scope_hash: "scope".to_owned(),
                ..Default::default()
            },
        );
    }

    let dir = TempDir::new().unwrap();
    let cursor_path = dir.path().join("spending.json");
    let provider_path = dir.path().join("provider-spending.json");
    let workspace_path = dir.path().join("workspace-spending.json");

    for (path, write_cache, expected_version) in [
        (
            cursor_path.as_path(),
            write_cursor as fn(&Path),
            SPENDING_CACHE_VERSION,
        ),
        (
            provider_path.as_path(),
            write_provider as fn(&Path),
            PROVIDER_SPENDING_VERSION,
        ),
        (
            workspace_path.as_path(),
            write_workspace as fn(&Path),
            WORKSPACE_SPENDING_VERSION,
        ),
    ] {
        write_cache(path);
        assert_eq!(peek_cache_version(path), Some(expected_version));

        let newer = br#"{"version":9999,"sentinel":true}"#.to_vec();
        std::fs::write(path, &newer).unwrap();
        write_cache(path);
        assert_eq!(std::fs::read(path).unwrap(), newer);

        let current = format!(r#"{{"version":{expected_version},"sentinel":true}}"#).into_bytes();
        std::fs::write(path, &current).unwrap();
        write_cache(path);
        assert_eq!(peek_cache_version(path), Some(expected_version));
        assert_ne!(std::fs::read(path).unwrap(), current);
    }

    assert_eq!(peek_cache_version(&dir.path().join("missing.json")), None);
    let no_version = dir.path().join("no-version.json");
    std::fs::write(&no_version, br#"{"refreshed_at_ms":123,"version":9999}"#).unwrap();
    assert_eq!(peek_cache_version(&no_version), None);
}

#[test]
fn spending_cursor_cache_wire_shape_uses_short_keys() {
    let full = CachedEntry {
        ts_secs: 12_345,
        cost_usd: 0.125,
        input: 10,
        output: 20,
        cache_write: 3,
        cache_read: 4,
        tool_calls: BTreeMap::from([("Read".to_owned(), 2)]),
        message_id: Some("msg-1".to_owned()),
        request_id: Some("req-1".to_owned()),
        dedup_key: None,
        thread_id: Some("thread-1".to_owned()),
        is_sidechain: true,
        has_speed: true,
        model: Some("claude-opus-4-8".to_owned()),
        rolled: true,
    };
    let codex = CachedEntry {
        ts_secs: 67_890,
        cost_usd: 0.001,
        input: 100,
        output: 50,
        cache_write: 0,
        cache_read: 25,
        tool_calls: Default::default(),
        message_id: None,
        request_id: None,
        dedup_key: Some("codex:event".to_owned()),
        thread_id: None,
        is_sidechain: false,
        has_speed: false,
        model: Some("gpt-5-codex".to_owned()),
        rolled: false,
    };
    let entries = vec![full.clone(), codex.clone()];
    let value = serde_json::to_value(&entries).unwrap();

    insta::assert_json_snapshot!(value, @r###"
        [
          {
            "d": true,
            "h": "thread-1",
            "i": 10,
            "l": "claude-opus-4-8",
            "m": "msg-1",
            "n": {
              "Read": 2
            },
            "o": 20,
            "q": "req-1",
            "r": 4,
            "s": true,
            "t": 12345,
            "u": 0.125,
            "v": true,
            "w": 3
          },
          {
            "i": 100,
            "k": "codex:event",
            "l": "gpt-5-codex",
            "o": 50,
            "r": 25,
            "t": 67890,
            "u": 0.001
          }
        ]
        "###);
    assert_eq!(
        serde_json::from_value::<Vec<CachedEntry>>(value).unwrap(),
        entries
    );

    let file = FileCacheEntry {
        stat: crate::agents::TranscriptStat {
            mtime_secs: 88_888,
            mtime_nanos: 999,
            len: 123,
            companion: Some(crate::agents::TranscriptCompanionStat {
                mtime_secs: 88_889,
                mtime_nanos: 111,
                len: 456,
            }),
        },
        cursor: SpendCursor {
            offset: 77,
            state: Some(serde_json::json!({"acc": 3})),
        },
        origin_path: Some(PathBuf::from("/tmp/repo")),
        entries: vec![codex],
        unknown_models: BTreeMap::from([("new-model".to_owned(), 66_666)]),
    };
    let file_value = serde_json::to_value(&file).unwrap();
    assert!(file_value.get("mtime_secs").is_none());
    assert_eq!(file_value["s"]["mtime_secs"], 88_888);
    assert_eq!(file_value["s"]["mtime_nanos"], 999);
    assert_eq!(file_value["s"]["len"], 123);
    assert_eq!(file_value["s"]["companion"]["mtime_secs"], 88_889);
    assert_eq!(file_value["s"]["companion"]["mtime_nanos"], 111);
    assert_eq!(file_value["s"]["companion"]["len"], 456);
    assert_eq!(file_value["c"]["o"], 77);
    assert_eq!(file_value["c"]["s"], serde_json::json!({"acc": 3}));
    assert_eq!(
        serde_json::from_value::<FileCacheEntry>(file_value).unwrap(),
        file
    );
}
