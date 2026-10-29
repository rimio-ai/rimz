use super::*;

fn set(models: &[&str]) -> BTreeSet<String> {
    models.iter().map(|model| (*model).to_owned()).collect()
}

fn book() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{
            "gpt-5": {"input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5},
            "claude-sonnet-4-20250514": {"input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5},
            "claude-sonnet-4": {"input_cost_per_token": 9e-9, "output_cost_per_token": 9e-9}
        }"#,
    )
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
    assert!((book().price("gpt-5").unwrap().input - 1.25e-6).abs() < 1e-18);

    // The dated key is longer than the bare key, so it wins the scan.
    let p = book()
        .price("claude-sonnet-4-20250514-via-bedrock")
        .unwrap();
    assert!((p.input - 3e-6).abs() < 1e-18);

    let b = PriceBook::from_litellm_json(
        r#"{"gpt-5-codex": {"input_cost_per_token": 2e-6, "output_cost_per_token": 4e-6}}"#,
    );
    // `gpt-5.codex@v1` normalizes to `gpt-5-codex-v1`, matching `gpt-5-codex`.
    assert!(b.price("gpt-5.codex@v1").is_some());

    // `gpt-5-9` must not be priced as `gpt-5`.
    assert!(book().price("gpt-5-9").is_none());
    assert!(book().price("totally-unknown-model").is_none());
}

#[test]
fn fuzzy_memo_is_transparent_and_survives_a_clone() {
    let book = book();

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
    let cache = PricingCache::default();
    assert!(!should_refresh(
        &cache,
        unix_secs_now(),
        true,
        &BTreeSet::new()
    ));

    let cache = PricingCache::default();
    assert!(should_refresh(
        &cache,
        unix_secs_now(),
        false,
        &BTreeSet::new()
    ));

    let now = unix_secs_now();
    let cache = PricingCache {
        fetched_at_secs: now,
        last_attempt_secs: now,
        ..Default::default()
    };
    assert!(!should_refresh(&cache, now, false, &BTreeSet::new()));

    // Data is stale (never fetched) but we just attempted: back off.
    let cache = PricingCache {
        fetched_at_secs: 0,
        last_attempt_secs: now,
        ..Default::default()
    };
    assert!(!should_refresh(&cache, now, false, &BTreeSet::new()));
}

#[test]
fn unknown_chase_refresh_policy_handles_seen_sets_and_offline_mode() {
    let now = 10 * UNKNOWN_REFRESH_TTL_SECS;
    let cache = PricingCache {
        fetched_at_secs: now,
        ..Default::default()
    };
    assert!(should_refresh(&cache, now, false, &set(&["new-model"])));
    assert!(!should_refresh(&cache, now, true, &set(&["new-model"])));

    let cache = PricingCache {
        fetched_at_secs: now,
        last_attempt_secs: now,
        ..Default::default()
    };
    assert!(!should_refresh(&cache, now, false, &set(&["new-model"])));

    let cache = PricingCache {
        fetched_at_secs: now,
        unknown_attempt_secs: now - UNKNOWN_REFRESH_TTL_SECS - 1,
        unknown_backoff_secs: 2 * UNKNOWN_REFRESH_TTL_SECS,
        unknown_seen: set(&["new-model"]),
        ..Default::default()
    };

    assert!(!should_refresh(&cache, now, false, &set(&["new-model"])));
    assert!(should_refresh(
        &cache,
        now + UNKNOWN_REFRESH_TTL_SECS,
        false,
        &set(&["new-model"])
    ));

    let cache = PricingCache {
        fetched_at_secs: now,
        unknown_attempt_secs: now - UNKNOWN_REFRESH_TTL_SECS - 1,
        unknown_backoff_secs: 4 * UNKNOWN_REFRESH_TTL_SECS,
        unknown_seen: set(&["old-model"]),
        ..Default::default()
    };

    assert!(should_refresh(
        &cache,
        now,
        false,
        &set(&["old-model", "new-model"])
    ));
}

#[test]
fn chase_attempts_double_until_the_cap_and_clear_when_healed() {
    let mut cache = PricingCache::default();

    note_chase_attempt(&mut cache, 1, set(&["new-model"]));
    assert_eq!(cache.unknown_backoff_secs, UNKNOWN_REFRESH_TTL_SECS);

    note_chase_attempt(&mut cache, 2, set(&["new-model"]));
    assert_eq!(cache.unknown_backoff_secs, 2 * UNKNOWN_REFRESH_TTL_SECS);

    cache.unknown_backoff_secs = UNKNOWN_BACKOFF_CAP_SECS;
    note_chase_attempt(&mut cache, 3, set(&["new-model"]));
    assert_eq!(cache.unknown_backoff_secs, UNKNOWN_BACKOFF_CAP_SECS);

    let mut cache = PricingCache {
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

    let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
    let litellm_fetches = Cell::new(0);
    let models_dev_fetches = Cell::new(0);
    let mut cache = PricingCache::default();
    let book = refresh_cache(
        &mut cache,
        now,
        BTreeSet::new(),
        &BTreeSet::new(),
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
    let grok = book.price("grok-4.5").unwrap();
    assert_eq!(grok.long_context_threshold, Some(200_000));
    assert_eq!(grok.max_input_tokens, Some(500_000));

    let previous_models = cache.models.clone();
    let previous_fetched_at = cache.fetched_at_secs;
    let failed_at = now + RETRY_BACKOFF_SECS + 1;
    let book = refresh_cache(
        &mut cache,
        failed_at,
        BTreeSet::new(),
        &BTreeSet::new(),
        || Some(include_str!("tests/fixtures/litellm.json").to_owned()),
        || None,
    );

    assert_eq!(cache.models, previous_models);
    assert_eq!(cache.fetched_at_secs, previous_fetched_at);
    assert_eq!(cache.last_attempt_secs, failed_at);
    assert_eq!(
        book.price("grok-4.5").unwrap().long_context_threshold,
        Some(200_000)
    );

    // A LiteLLM outage already rules the attempt out, so the 3MB
    // models.dev download never starts.
    let skipped = Cell::new(0);
    refresh_cache(
        &mut cache,
        failed_at + RETRY_BACKOFF_SECS + 1,
        BTreeSet::new(),
        &BTreeSet::new(),
        || None,
        || {
            skipped.set(skipped.get() + 1);
            Some(include_str!("tests/fixtures/models-dev.json").to_owned())
        },
    );

    assert_eq!(skipped.get(), 0);
    assert_eq!(cache.models, previous_models);
}

#[test]
fn unpriced_subset_filters_exact_and_fuzzy_priced_unknowns() {
    let b = PriceBook::from_litellm_json(
        r#"{"new-model": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}"#,
    );

    assert!(unpriced_subset(&b, &set(&["new-model"])).is_empty());
    assert!(unpriced_subset(&b, &set(&["new-model-via-provider"])).is_empty());
}

#[test]
fn exact_lookup_does_not_borrow_a_boundary_prefix() {
    let book = PriceBook::from_litellm_json(
        r#"{"exact-model":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"max_input_tokens":123456}}"#,
    );

    assert_eq!(
        book.exact_price(" exact-model ").unwrap().max_input_tokens,
        Some(123_456)
    );
    assert!(book.price("exact-model-via-gateway").is_some());
    assert!(book.exact_price("exact-model-via-gateway").is_none());
}

#[test]
fn assembly_overwrites_embedded_rows_with_the_projected_cache() {
    let cache = PricingCache {
        models: BTreeMap::from([
            (
                "gpt-5".to_owned(),
                Pricing {
                    input: 9e-6,
                    output: 9e-6,
                    ..Pricing::empty()
                },
            ),
            (
                "models-dev-only".to_owned(),
                Pricing {
                    input: 7e-6,
                    output: 7e-6,
                    ..Pricing::empty()
                },
            ),
        ]),
        ..Default::default()
    };

    let book = PriceBook::assembled(&cache);

    assert!((book.price("gpt-5").unwrap().input - 9e-6).abs() < 1e-18);
    assert!((book.price("models-dev-only").unwrap().input - 7e-6).abs() < 1e-18);
}

#[test]
fn cached_book_reads_shared_cache_without_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pricing-cache.json");
    let model = "rimz-test-cached-model";
    let cache = PricingCache {
        models: BTreeMap::from([(
            model.to_owned(),
            Pricing {
                input: 3e-6,
                output: 15e-6,
                cache_read: 3e-7,
                cache_create: 3.75e-6,
                max_input_tokens: Some(777_000),
                ..Pricing::empty()
            },
        )]),
        ..Default::default()
    };
    write_cache(&path, &cache);

    let book = cached_book(&path);
    let price = book.price(model).expect("cached price");

    assert!((price.input - 3e-6).abs() < f64::EPSILON);
    assert!((price.output - 15e-6).abs() < f64::EPSILON);
    assert_eq!(price.max_input_tokens, Some(777_000));
}

#[test]
fn cached_book_invalidates_when_shared_cache_stamp_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pricing-cache.json");
    let model = "rimz-test-changing-model";
    let mut cache = PricingCache {
        models: BTreeMap::from([(
            model.to_owned(),
            Pricing {
                input: 3e-6,
                output: 15e-6,
                ..Pricing::empty()
            },
        )]),
        ..Default::default()
    };
    write_cache(&path, &cache);

    let (first, first_fingerprint) = cached_book_with_fingerprint(&path);
    assert!((first.price(model).unwrap().input - 3e-6).abs() < f64::EPSILON);

    cache.models.get_mut(model).unwrap().input = 30e-6;
    cache.models.insert(
        "rimz-test-length-bump".to_owned(),
        Pricing {
            input: 1e-6,
            output: 1e-6,
            ..Pricing::empty()
        },
    );
    write_cache(&path, &cache);

    let (second, second_fingerprint) = cached_book_with_fingerprint(&path);
    assert!((second.price(model).unwrap().input - 30e-6).abs() < f64::EPSILON);
    assert_ne!(first_fingerprint, second_fingerprint);
}

#[test]
fn schema_three_pricing_cache_drops_the_split_source_shape() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pricing-cache.json");
    std::fs::write(
        &path,
        r#"{"schema":3,"litellm":{"rimz-test-stale-model":{"input":999.0,"output":999.0}}}"#,
    )
    .unwrap();

    let cache = read_cache(&path);

    assert_eq!(cache.schema, PRICING_CACHE_SCHEMA);
    assert!(cache.models.is_empty());
}
