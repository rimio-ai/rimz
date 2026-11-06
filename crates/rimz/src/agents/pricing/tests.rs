use super::*;

fn set(models: &[&str]) -> BTreeSet<String> {
    models.iter().map(|model| (*model).to_owned()).collect()
}

const BOOK_JSON: &str = r#"{
            "gpt-5": {"input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5},
            "claude-sonnet-4-20250514": {"input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5},
            "claude-sonnet-4": {"input_cost_per_token": 9e-9, "output_cost_per_token": 9e-9}
        }"#;

fn book() -> PriceBook {
    PriceBook::from_litellm_json(BOOK_JSON)
}

fn books(json: &str) -> [PriceBook; 2] {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prices.sqlite");
    testkit::seed_cache_at(&path, json);
    [
        PriceBook::from_litellm_json(json),
        (*cached_book(&path)).clone(),
    ]
}

#[test]
fn cache_rewarm_prices_the_observed_tier_without_guessing_unknown_models() {
    let book = PriceBook::fixture();
    let cost = |model, retention| cache_rewarm_cost_usd(&book, model, 100000, retention);
    assert_eq!(
        cost("claude-sonnet-4-6", Some(CacheWriteRetention::OneHour)),
        Some(0.6)
    );
    assert_eq!(
        cost("claude-sonnet-4-6", Some(CacheWriteRetention::FiveMinutes)),
        Some(0.375)
    );
    assert_eq!(cost("gpt-5", None), Some(0.125));
    assert_eq!(
        cost("future-unpriced-model", Some(CacheWriteRetention::OneHour)),
        None
    );
    assert_eq!(cost("<synthetic>", None), None);
}

#[test]
fn base_rates_own_cache_defaults_and_explicitness() {
    let defaulted = Pricing::from_base_rates(2.0, 4.0, None, None);
    assert_eq!(defaulted.cache_create, 2.5);
    assert_eq!(defaulted.cache_read, 0.2);
    assert!(!defaulted.cache_read_explicit);

    let explicit = Pricing::from_base_rates(2.0, 4.0, Some(3.0), Some(0.5));
    assert_eq!(explicit.cache_create, 3.0);
    assert_eq!(explicit.cache_read, 0.5);
    assert!(explicit.cache_read_explicit);
}

#[test]
fn price_lookup_handles_exact_fuzzy_embedded_and_unknown_boundaries() {
    for book in books(BOOK_JSON) {
        assert!((book.price("gpt-5").unwrap().input - 1.25e-6).abs() < 1e-18);

        // The dated key is longer than the bare key, so it wins.
        let p = book.price("claude-sonnet-4-20250514-via-bedrock").unwrap();
        assert!((p.input - 3e-6).abs() < 1e-18);

        // A purely numeric version bump must not borrow the base rate.
        assert!(book.price("gpt-5-9").is_none());
        assert!(book.price("totally-unknown-model").is_none());
    }
    for b in
        books(r#"{"gpt-5-codex": {"input_cost_per_token": 2e-6, "output_cost_per_token": 4e-6}}"#)
    {
        assert!(b.price("gpt-5.codex@v1").is_some());
    }
}

#[test]
fn scoped_rows_do_not_price_bare_other_provider_or_shorter_ids() {
    for key in ["opencode_go/hy4", "opencode_go/hy4-preview"] {
        let json = serde_json::json!({key: {
            "input_cost_per_token": 1e-6,
            "output_cost_per_token": 2e-6
        }})
        .to_string();
        for book in books(&json) {
            assert_eq!(
                ["hy4", "opencode/hy4", "opencode_go/hy4", key]
                    .map(|model| book.price(model).map(|price| price.input)),
                [
                    None,
                    None,
                    (key == "opencode_go/hy4").then_some(1e-6),
                    Some(1e-6)
                ]
            );
        }
    }
}

#[test]
fn fuzzy_memo_is_transparent_and_survives_a_clone() {
    for book in books(BOOK_JSON) {
        // Repeated fuzzy lookups (hit then cached) stay identical, and a cached
        // miss keeps reading as a miss rather than flipping to a stale hit.
        let first = book.price("claude-sonnet-4-20250514-via-bedrock").unwrap();
        let second = book.price("claude-sonnet-4-20250514-via-bedrock").unwrap();
        assert_eq!(first, second);
        assert!((second.input - 3e-6).abs() < 1e-18);
        assert!(book.price("totally-unknown-model").is_none());
        assert!(book.price("totally-unknown-model").is_none());

        // A clone shares entries, so a fuzzy lookup on it resolves the same way
        // whether or not the original primed the memo first.
        let clone = book.clone();
        assert_eq!(
            clone.price("claude-sonnet-4-20250514-via-bedrock").unwrap(),
            first
        );
        assert!(clone.price("gpt-5-9").is_none());
    }
}

#[test]
fn openai_long_context_pricing_switches_the_whole_request_above_272k() {
    let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
    assert_eq!(price.long_context_threshold, Some(272_000));

    let short = price.cost_of(TokenSplit::new(100_000, 1_000).cached(0, 100));
    assert!((short - 0.53005).abs() < 1e-9, "short cost was {short}");

    let long = price.cost_of(TokenSplit::new(300_000, 1_000).cached(0, 100));
    assert!((long - 3.0451).abs() < 1e-9, "long cost was {long}");
}

#[test]
fn list_rates_preserve_request_tiers_marginal_tiers_and_fast_pricing() {
    let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for price in [
        price,
        Pricing {
            long_context_threshold: None,
            ..price
        },
    ] {
        for input in [1_000, 272_000, 300_000] {
            for fast in [false, true] {
                let split = TokenSplit::new(input, 1_000).cached(100, 200).fast(fast);
                actual.push(price.cost_of_with_rates(split, PricingRates::List));
                expected.push(price.cost_of(split));
            }
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn base_rates_ignore_all_long_context_tiers_and_replace_the_fast_multiplier() {
    let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
    let split = TokenSplit {
        input: 300_000,
        output: 300_000,
        cache_write: 300_000,
        cache_write_1h: 300_000,
        cache_read: 300_000,
        fast: false,
    };
    let mut actual = Vec::new();
    for price in [
        price,
        Pricing {
            long_context_threshold: None,
            ..price
        },
    ] {
        for (fast, fast_multiplier) in [(false, 2.5), (true, 2.5), (true, 2.0)] {
            actual.push(
                price.cost_of_with_rates(split.fast(fast), PricingRates::Base { fast_multiplier }),
            );
        }
    }
    let expected = [15.525, 38.8125, 31.05, 15.525, 38.8125, 31.05];
    assert_eq!(
        actual
            .iter()
            .map(|cost| (cost * 1e9).round() as u64)
            .collect::<Vec<_>>(),
        expected.map(|cost| (cost * 1e9_f64).round() as u64),
    );
}

#[test]
fn session_cost_uses_base_rates_above_request_tier_boundaries() {
    let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
    let cost = price.session_cost(500_000, 10_000, 20_000, 400_000);
    let expected = 500_000.0 * price.input
        + 10_000.0 * price.output
        + 20_000.0 * price.cache_create
        + 400_000.0 * price.cache_read;
    assert!((cost - expected).abs() < f64::EPSILON);
    let split = TokenSplit::new(500_000, 10_000).cached(20_000, 400_000);
    assert_ne!(cost, price.cost_of(split.fast(true)));
}

#[test]
fn refresh_policy_covers_offline_empty_fresh_and_recent_failures() {
    let cache = RefreshState::default();
    assert!(!should_refresh(
        &cache,
        0,
        unix_secs_now(),
        true,
        &BTreeSet::new()
    ));
    assert!(should_refresh(
        &cache,
        0,
        unix_secs_now(),
        false,
        &BTreeSet::new()
    ));

    let now = unix_secs_now();
    let cache = RefreshState {
        last_attempt_secs: now,
        ..Default::default()
    };
    assert!(!should_refresh(&cache, now, now, false, &BTreeSet::new()));

    // Data is stale (never fetched) but we just attempted: back off.
    assert!(!should_refresh(&cache, 0, now, false, &BTreeSet::new()));
}

#[test]
fn unknown_chase_refresh_policy_handles_seen_sets_and_offline_mode() {
    let now = 10 * UNKNOWN_REFRESH_TTL_SECS;
    let cache = RefreshState::default();
    assert!(should_refresh(
        &cache,
        now,
        now,
        false,
        &set(&["new-model"])
    ));
    assert!(!should_refresh(
        &cache,
        now,
        now,
        true,
        &set(&["new-model"])
    ));

    let cache = RefreshState {
        last_attempt_secs: now,
        ..Default::default()
    };
    assert!(!should_refresh(
        &cache,
        now,
        now,
        false,
        &set(&["new-model"])
    ));

    let cache = RefreshState {
        unknown_attempt_secs: now - UNKNOWN_REFRESH_TTL_SECS - 1,
        unknown_backoff_secs: 2 * UNKNOWN_REFRESH_TTL_SECS,
        unknown_seen: set(&["new-model"]),
        ..Default::default()
    };

    assert!(!should_refresh(
        &cache,
        now,
        now,
        false,
        &set(&["new-model"])
    ));
    assert!(should_refresh(
        &cache,
        now,
        now + UNKNOWN_REFRESH_TTL_SECS,
        false,
        &set(&["new-model"])
    ));

    let cache = RefreshState {
        unknown_attempt_secs: now - UNKNOWN_REFRESH_TTL_SECS - 1,
        unknown_backoff_secs: 4 * UNKNOWN_REFRESH_TTL_SECS,
        unknown_seen: set(&["old-model"]),
        ..Default::default()
    };

    assert!(should_refresh(
        &cache,
        now,
        now,
        false,
        &set(&["old-model", "new-model"])
    ));
}

#[test]
fn chase_attempts_double_until_the_cap_and_clear_when_healed() {
    let mut cache = RefreshState::default();

    note_chase_attempt(&mut cache, 1, set(&["new-model"]));
    assert_eq!(cache.unknown_backoff_secs, UNKNOWN_REFRESH_TTL_SECS);

    note_chase_attempt(&mut cache, 2, set(&["new-model"]));
    assert_eq!(cache.unknown_backoff_secs, 2 * UNKNOWN_REFRESH_TTL_SECS);

    cache.unknown_backoff_secs = UNKNOWN_BACKOFF_CAP_SECS;
    note_chase_attempt(&mut cache, 3, set(&["new-model"]));
    assert_eq!(cache.unknown_backoff_secs, UNKNOWN_BACKOFF_CAP_SECS);

    let mut cache = RefreshState {
        unknown_attempt_secs: 10,
        unknown_backoff_secs: UNKNOWN_REFRESH_TTL_SECS,
        unknown_seen: set(&["new-model"]),
        ..Default::default()
    };

    note_chase_attempt(&mut cache, 20, BTreeSet::new());

    assert_eq!(cache.unknown_attempt_secs, 0);
    assert_eq!(cache.unknown_backoff_secs, 0);
    assert!(cache.unknown_seen.is_empty());
}

#[test]
fn runtime_refresh_projects_both_sources_and_keeps_the_last_good_table() {
    use std::cell::Cell;
    use std::os::unix::fs::MetadataExt;

    let (_root, runtime) = isolated_runtime();
    let path = runtime.shared_pricing_table_path();
    let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
    let litellm_fetches = Cell::new(0);
    let models_dev_fetches = Cell::new(0);
    let first = load_for_spending_with(
        &runtime,
        &BTreeSet::new(),
        || now,
        false,
        HashMap::new,
        || {
            litellm_fetches.set(litellm_fetches.get() + 1);
            Some(include_str!("tests/fixtures/litellm.json").to_owned())
        },
        || {
            models_dev_fetches.set(models_dev_fetches.get() + 1);
            Some(include_str!("tests/fixtures/models-dev.json").to_owned())
        },
    );
    assert_eq!(litellm_fetches.get(), 1);
    assert_eq!(models_dev_fetches.get(), 1);
    let grok = first.price("grok-4.5").unwrap();
    assert_eq!(grok.long_context_threshold, Some(200_000));
    assert_eq!(grok.max_input_tokens, Some(500_000));

    let table = sqlite::Table::open(&path).unwrap();
    let previous_models = table.rows().unwrap();
    let previous_fetched_at = table.fetched_at_secs;
    let inode = fs::metadata(&path).unwrap().ino();
    let failed_at = now + REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
    let book = load_for_spending_with(
        &runtime,
        &BTreeSet::new(),
        || failed_at,
        false,
        || panic!("an existing table never loads a seed"),
        || Some(include_str!("tests/fixtures/litellm.json").to_owned()),
        || None,
    );
    let table = sqlite::Table::open(&path).unwrap();
    let state: RefreshState =
        crate::disk::atomic::read_json_cache(&runtime.shared_pricing_refresh_path());
    assert_eq!(table.rows().unwrap(), previous_models);
    assert_eq!(table.fetched_at_secs, previous_fetched_at);
    assert_eq!(state.last_attempt_secs, failed_at);
    assert_eq!(
        book.price("grok-4.5").unwrap().long_context_threshold,
        Some(200_000)
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert!(Arc::ptr_eq(&first, &book));

    // A LiteLLM outage skips the models.dev download.
    let skipped = Cell::new(0);
    load_for_spending_with(
        &runtime,
        &BTreeSet::new(),
        || failed_at + RETRY_BACKOFF_SECS + 1,
        false,
        || panic!("an existing table never loads a seed"),
        || None,
        || {
            skipped.set(skipped.get() + 1);
            Some(include_str!("tests/fixtures/models-dev.json").to_owned())
        },
    );
    assert_eq!(skipped.get(), 0);
    assert_eq!(
        sqlite::Table::open(&path).unwrap().rows().unwrap(),
        previous_models
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
}

#[test]
fn unpriced_subset_filters_exact_and_fuzzy_priced_unknowns() {
    for b in
        books(r#"{"new-model": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}"#)
    {
        assert!(unpriced_subset(&b, &set(&["new-model"])).is_empty());
        assert!(unpriced_subset(&b, &set(&["new-model-via-provider"])).is_empty());
    }
}

#[test]
fn exact_lookup_does_not_borrow_a_boundary_prefix() {
    for book in books(
        r#"{"exact-model":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"max_input_tokens":123456}}"#,
    ) {
        assert_eq!(
            book.exact_price(" exact-model ").unwrap().max_input_tokens,
            Some(123_456)
        );
        assert!(book.price("exact-model-via-gateway").is_some());
        assert!(book.exact_price("exact-model-via-gateway").is_none());
    }
}

fn isolated_runtime() -> (tempfile::TempDir, crate::RuntimePaths) {
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(root.path()),
        root.path(),
    )
    .unwrap();
    (root, runtime)
}

fn assert_sqlite(path: &Path) {
    assert!(
        fs::read(path).is_ok_and(|bytes| bytes.starts_with(b"SQLite format 3\0")),
        "the shared price cache must be a usable SQLite table: {}",
        path.display()
    );
}

#[test]
fn sqlite_and_memory_books_agree_for_stored_keys_and_boundary_probes() {
    let mut document: serde_json::Value =
        serde_json::from_str(include_str!("tests/fixtures/prices.json")).unwrap();
    document["full-u64-capacity"] = serde_json::json!({
        "input_cost_per_token": 1.0, "output_cost_per_token": 2.0,
        "max_input_tokens": u64::MAX, "long_context_threshold": u64::MAX,
    });
    let json = serde_json::to_string(&document).unwrap();
    let json = json.as_str();
    let memory = PriceBook::from_litellm_json(json);
    let (root, runtime) = isolated_runtime();
    let path = testkit::seed_cache(&runtime, json);
    assert_sqlite(&path);
    let disk = cached_book(&path);
    let mut probes: Vec<String> = embedded::parse(json).into_keys().collect();
    probes.extend(
        [
            "GPT-5",
            "gpt@5",
            "gpt.5",
            "gpt-5-20260101",
            "gpt-5-9",
            "gpt-5-via-bedrock",
            " gpt-5 ",
            "totally-unknown-model",
            "",
            "claude-sonnet-4-6-20260101-via-bedrock",
            "gpt-5-é",
            "gpt-5-2026010",
        ]
        .map(str::to_owned),
    );
    for probe in probes {
        assert_eq!(disk.price(&probe), memory.price(&probe), "price({probe:?})");
        assert_eq!(
            disk.exact_price(&probe),
            memory.exact_price(&probe),
            "exact({probe:?})"
        );
    }
    assert!(!runtime.shared_pricing_refresh_path().exists());
    drop(root);
}

#[test]
fn fuzzy_ties_choose_the_smallest_raw_key_on_fresh_books() {
    let json = r#"{"Foo.1":{"input_cost_per_token":1.0,"output_cost_per_token":2.0},"foo-1":{"input_cost_per_token":9.0,"output_cost_per_token":9.0}}"#;
    let (_root, runtime) = isolated_runtime();
    let path = testkit::seed_cache(&runtime, json);
    let mut actual = Vec::new();
    for _ in 0..32 {
        testkit::reset_cached_book();
        actual.push(
            PriceBook::from_litellm_json(json)
                .price("FOO-1-via-provider")
                .unwrap()
                .input,
        );
        actual.push(
            cached_book(&path)
                .price("FOO-1-via-provider")
                .unwrap()
                .input,
        );
    }
    assert_eq!(actual, vec![1.0; 64]);
}

#[test]
fn walker_seeds_a_missing_table_offline_and_after_a_failed_fetch() {
    let seed = r#"{"seed-only":{"input_cost_per_token":1.0,"output_cost_per_token":2.0}}"#;
    let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
    let mut actual = Vec::new();
    for offline in [true, false] {
        let (_root, runtime) = isolated_runtime();
        let book = load_for_spending_with(
            &runtime,
            &BTreeSet::new(),
            || now,
            offline,
            || embedded::parse(seed),
            || None,
            || panic!("a failed LiteLLM fetch must not fetch models.dev"),
        );
        let path = runtime.shared_pricing_table_path();
        let table = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok();
        let count = table.as_ref().and_then(|connection| {
            connection
                .query_row("SELECT count(*) FROM prices", [], |row| {
                    row.get::<_, i64>(0)
                })
                .ok()
        });
        let fetched = table.as_ref().and_then(|connection| {
            connection
                .query_row(
                    "SELECT value FROM meta WHERE name = 'fetched_at_secs'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
        });
        let state = fs::read(runtime.shared_pricing_refresh_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let attempted = state.and_then(|state| state["last_attempt_secs"].as_u64());
        actual.push((
            book.price("seed-only").map(|price| price.input),
            count,
            fetched,
            attempted,
        ));
    }
    assert_eq!(
        actual,
        [
            (Some(1.0), Some(1), Some(0), None),
            (Some(1.0), Some(1), Some(0), Some(now))
        ]
    );
}

#[test]
fn walker_skips_empty_seeds_and_publishes_when_rows_arrive() {
    let seed = r#"{"seed-only":{"input_cost_per_token":1.0,"output_cost_per_token":2.0}}"#;
    let unknowns = set(&["seed-only"]);
    let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
    for offline in [true, false] {
        let (_root, runtime) = isolated_runtime();
        let path = runtime.shared_pricing_table_path();
        let fallback = load_for_spending_with(
            &runtime,
            &unknowns,
            || now,
            offline,
            HashMap::new,
            || None,
            || panic!("a failed LiteLLM fetch must not fetch models.dev"),
        );
        assert!(!path.exists(), "an empty seed must not publish a table");
        let (reader, fingerprint) = cached_book_with_fingerprint(&path);
        assert!(fingerprint.is_none());
        assert!(Arc::ptr_eq(&fallback, &reader));
        let state: RefreshState =
            crate::disk::atomic::read_json_cache(&runtime.shared_pricing_refresh_path());
        let expected_state = if offline {
            RefreshState::default()
        } else {
            RefreshState {
                last_attempt_secs: now,
                unknown_attempt_secs: now,
                unknown_backoff_secs: UNKNOWN_REFRESH_TTL_SECS,
                unknown_seen: unknowns.clone(),
            }
        };
        assert_eq!(state, expected_state);

        let book = load_for_spending_with(
            &runtime,
            &unknowns,
            || now + 1,
            true,
            || embedded::parse(seed),
            || panic!("offline"),
            || panic!("offline"),
        );
        let table = sqlite::Table::open(&path).unwrap();
        assert_eq!(table.rows().unwrap(), embedded::parse(seed));
        assert_eq!(table.fetched_at_secs, 0);
        assert_eq!(book.price("seed-only").unwrap().input, 1.0);
        let state: RefreshState =
            crate::disk::atomic::read_json_cache(&runtime.shared_pricing_refresh_path());
        assert_eq!(
            state,
            RefreshState {
                last_attempt_secs: expected_state.last_attempt_secs,
                ..Default::default()
            }
        );
    }
}

#[test]
fn refresh_carries_previous_rows_and_projection_wins_without_loading_a_new_seed() {
    let (_root, runtime) = isolated_runtime();
    testkit::seed_cache_at(
        &runtime.shared_pricing_table_path(),
        r#"{"kept-only":{"input_cost_per_token":7.0,"output_cost_per_token":8.0},"gpt-5":{"input_cost_per_token":9.0,"output_cost_per_token":9.0}}"#,
    );
    let book = load_for_spending_with(
        &runtime,
        &BTreeSet::new(),
        || REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1,
        false,
        || panic!("a usable table must not load a seed"),
        || Some(include_str!("tests/fixtures/litellm.json").to_owned()),
        || Some(include_str!("tests/fixtures/models-dev.json").to_owned()),
    );
    assert_eq!(book.price("kept-only").map(|price| price.input), Some(7.0));
    let projected = source::project_sources(
        include_str!("tests/fixtures/litellm.json"),
        Some(include_str!("tests/fixtures/models-dev.json")),
    )
    .unwrap()
    .0;
    let expected = PriceBook::from_litellm_json(&serde_json::to_string(&projected).unwrap());
    assert_eq!(book.price("gpt-5"), expected.price("gpt-5"));
    assert_sqlite(&runtime.shared_pricing_table_path());
}

#[test]
fn equal_length_same_second_replacements_invalidate_the_memo_and_fingerprint() {
    let (_root, runtime) = isolated_runtime();
    let path = runtime.shared_pricing_table_path();
    let mut fingerprints = Vec::new();
    let mut books = Vec::new();
    let mut lengths = Vec::new();
    let time = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    for rate in [1.0, 2.0] {
        testkit::seed_cache_at(
            &path,
            &serde_json::json!({"identity-model": {
                "input_cost_per_token": rate,
                "output_cost_per_token": 3.0,
                "cache_creation_input_token_cost": 0.0,
                "cache_read_input_token_cost": 0.0,
            }})
            .to_string(),
        );
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(time))
            .unwrap();
        lengths.push(fs::metadata(&path).unwrap().len());
        let (book, fingerprint) = cached_book_with_fingerprint(&path);
        fingerprints.push(fingerprint);
        books.push(book);
    }
    assert_eq!(lengths[0], lengths[1]);
    assert_eq!(
        books
            .iter()
            .map(|book| book.price("identity-model").unwrap().input)
            .collect::<Vec<_>>(),
        [1.0, 2.0]
    );
    assert!(!Arc::ptr_eq(&books[0], &books[1]));
    assert!(fingerprints.iter().all(Option::is_some));
    assert_ne!(fingerprints[0], fingerprints[1]);
}

#[test]
fn unusable_files_have_no_fingerprint_and_are_rebuilt_from_the_seed() {
    let mut fingerprints = Vec::new();
    let mut repaired = Vec::new();
    for wrong_version in [false, true] {
        let (_root, runtime) = isolated_runtime();
        let path = runtime.shared_pricing_table_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if wrong_version {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch("PRAGMA user_version = 2; CREATE TABLE meta (name TEXT PRIMARY KEY, value); INSERT INTO meta VALUES ('fetched_at_secs', 1), ('build_id', 'other');").unwrap();
        } else {
            fs::write(&path, b"garbage").unwrap();
        }
        let (fallback, fingerprint) = cached_book_with_fingerprint(&path);
        fingerprints.push(fingerprint);
        let loads = testkit::book_loads();
        assert!(Arc::ptr_eq(&fallback, &cached_book(&path)));
        assert_eq!(
            testkit::book_loads(),
            loads,
            "unusable files are memoized too"
        );
        assert_eq!(
            fallback.price("gpt-5"),
            embedded::load().get("gpt-5").copied()
        );
        let book = load_for_spending_with(
            &runtime,
            &BTreeSet::new(),
            unix_secs_now,
            true,
            || {
                embedded::parse(
                    r#"{"repair-seed":{"input_cost_per_token":4.0,"output_cost_per_token":5.0}}"#,
                )
            },
            || panic!("offline"),
            || panic!("offline"),
        );
        repaired.push((
            book.price("repair-seed").map(|price| price.input),
            fs::read(&path).unwrap().starts_with(b"SQLite format 3\0"),
        ));
    }
    assert_eq!(fingerprints, [None, None]);
    assert_eq!(repaired, [(Some(4.0), true), (Some(4.0), true)]);
}

#[test]
fn immutable_open_encodes_path_metacharacters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("price%?# table.sqlite");
    testkit::seed_cache_at(&path, include_str!("tests/fixtures/prices.json"));
    assert_sqlite(&path);
    let (book, fingerprint) = cached_book_with_fingerprint(&path);
    assert!(fingerprint.is_some(), "file fingerprint: {fingerprint:?}");
    assert_eq!(book.price("gpt-5"), PriceBook::fixture().price("gpt-5"));
}

#[test]
fn failed_and_skipped_fetches_keep_the_inode_and_the_walkers_book() {
    use std::os::unix::fs::MetadataExt;

    let (_root, runtime) = isolated_runtime();
    let path = testkit::seed_cache(&runtime, include_str!("tests/fixtures/prices.json"));
    let before = fs::metadata(&path).unwrap().ino();
    let first = cached_book(&path);
    let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
    let failed = load_for_spending_with(
        &runtime,
        &BTreeSet::new(),
        || now,
        false,
        || panic!("a usable table must not read the snapshot"),
        || None,
        || panic!("a failed first source skips the second"),
    );
    let skipped = load_for_spending_with(
        &runtime,
        &BTreeSet::new(),
        || now + 1,
        false,
        || panic!("a usable table must not read the snapshot"),
        || panic!("the retry backoff must skip the fetch"),
        || panic!("the retry backoff must skip the fetch"),
    );
    assert_eq!(
        (
            Arc::ptr_eq(&first, &failed),
            Arc::ptr_eq(&failed, &skipped),
            fs::metadata(&path).unwrap().ino()
        ),
        (true, true, before)
    );
}

#[test]
fn books_are_thread_shareable_and_clones_share_the_result_memo() {
    fn assert_traits<T: Send + Sync + Clone>() {}
    assert_traits::<PriceBook>();
    let book = book();
    assert!(Arc::ptr_eq(&book.fuzzy_cache, &book.clone().fuzzy_cache));
}
