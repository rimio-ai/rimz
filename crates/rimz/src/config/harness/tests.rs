use super::*;

#[test]
fn agent_lifetime_precedence_and_sources() {
    use crate::agents::{AgentContext, AgentStatus};
    use crate::ids::AgentKind;
    let cases = [
        (
            "claude = '1h'",
            "claude",
            None,
            Some(CacheWriteRetention::FiveMinutes),
            Some(300),
            LifetimeSource::Setting,
        ),
        (
            "claude = 'off'",
            "claude",
            None,
            Some(CacheWriteRetention::FiveMinutes),
            Some(300),
            LifetimeSource::Setting,
        ),
        (
            "claude = 'off'",
            "claude",
            None,
            Some(CacheWriteRetention::Unsplit),
            None,
            LifetimeSource::Setting,
        ),
        (
            "codex = { subscription = '2h' }",
            "codex",
            Some(SignIn::Subscription),
            None,
            Some(7200),
            LifetimeSource::Setting,
        ),
        (
            "codex = { subscription = '2h' }",
            "codex",
            Some(SignIn::ApiKey),
            None,
            Some(1800),
            LifetimeSource::Default,
        ),
        (
            "codex = { subscription = '2h' }",
            "codex",
            None,
            None,
            Some(1800),
            LifetimeSource::Default,
        ),
        (
            "codex = '12h'",
            "codex",
            Some(SignIn::ApiKey),
            None,
            Some(43200),
            LifetimeSource::Setting,
        ),
        (
            "codex = '12h'",
            "codex",
            Some(SignIn::Subscription),
            None,
            Some(43200),
            LifetimeSource::Setting,
        ),
        ("", "amp", None, None, Some(3600), LifetimeSource::Fallback),
        (
            "fallback = 'off'",
            "amp",
            None,
            None,
            None,
            LifetimeSource::Fallback,
        ),
        (
            "",
            "claude",
            None,
            Some(CacheWriteRetention::OneHour),
            Some(3600),
            LifetimeSource::Default,
        ),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(setting, kind, sign_in, observed, _, _)| {
            toml::from_str::<HarnessConfig>(&format!("[prompt_cache_ttl]\n{setting}"))
                .map(|config| {
                    let mut agent = AgentState::seed(
                        AgentKind::new_unchecked(*kind),
                        "agent".into(),
                        AgentStatus::Idle,
                        jiff::Timestamp::UNIX_EPOCH,
                    );
                    agent.sign_in = *sign_in;
                    agent.context = Some(AgentContext {
                        last_cache_write: *observed,
                        ..AgentContext::default()
                    });
                    (
                        config
                            .prompt_cache_lifetime(&agent)
                            .map(|ttl| ttl.as_secs()),
                        config.configured_lifetime(&agent.kind, *sign_in).source,
                    )
                })
                .map_err(|error| error.to_string())
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, _, _, _, ttl, source)| Ok((*ttl, *source)))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn split_lifetimes_and_fallback_round_trip_and_reject_unknown_sign_ins() {
    let actual: Vec<_> = [
        "codex = '12h'\nclaude = 'off'",
        "codex = { subscription = '2h', api-key = 'off' }",
        "amp = { api-key = '5m' }\nfallback = '2h'",
        "fallback = 'off'",
        "codex = { weekly = '1h' }",
    ]
    .map(|setting| {
        let parsed = toml::from_str::<HarnessConfig>(&format!("[prompt_cache_ttl]\n{setting}"));
        match parsed {
            Ok(config) => (
                toml::from_str::<HarnessConfig>(&toml::to_string(&config).unwrap()).unwrap()
                    == config,
                false,
            ),
            Err(error) => (
                false,
                error.to_string().contains("subscription") && error.to_string().contains("api-key"),
            ),
        }
    })
    .into_iter()
    .collect();
    assert_eq!(
        actual,
        [
            (true, false),
            (true, false),
            (true, false),
            (true, false),
            (false, true)
        ]
    );
}

#[test]
fn profiles_accept_max_blocks_but_not_flags_or_invalid_durations() {
    for value in ["\"2h\"", "\"off\"", "{ max = \"2h\" }", "{ max = \"off\" }"] {
        let profile = toml::from_str::<crate::config::Profile>(&format!(
            "agent = 'claude'\nkeep-warm = {value}"
        ));
        assert!(profile.is_ok(), "keep-warm {value}: {profile:?}");
    }
    for value in [
        "true",
        "3",
        "{ max = '0s' }",
        "{ max = '2x' }",
        "{ typo = '2h' }",
    ] {
        assert!(
            toml::from_str::<crate::config::Profile>(&format!(
                "agent = 'claude'\nkeep-warm = {value}"
            ))
            .is_err(),
            "{value}"
        );
    }
}

#[test]
fn prompt_cache_margin_is_positive_and_validated_against_effective_limits() {
    let defaults = serde_json::to_value(HarnessConfig::default()).unwrap();
    assert_eq!(defaults["prompt_cache_margin"], "1m");
    for raw in ["90s", "30s"] {
        let config: HarnessConfig =
            toml::from_str(&format!("prompt_cache_margin = {raw:?}")).unwrap();
        let encoded = toml::to_string(&config).unwrap();
        assert_eq!(
            serde_json::to_value(&config).unwrap()["prompt_cache_margin"],
            raw
        );
        assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
    }
    let failures: Vec<_> = [
        (
            "prompt_cache_margin = \"off\"",
            "prompt_cache_margin",
            false,
        ),
        ("prompt_cache_margin = \"0s\"", "prompt_cache_margin", false),
        ("prompt_cache_margin = \"30m\"", "codex", true),
        ("prompt_cache_margin = \"2h\"", "fallback", true),
        (
            "prompt_cache_margin = \"10m\"\nprompt_cache_ttl = { codex = { api-key = \"5m\" } }",
            "codex api-key",
            true,
        ),
        (
            "prompt_cache_margin = \"2m\"\nprompt_cache_ttl = { claude = \"2m\" }",
            "claude",
            true,
        ),
        (
            "prompt_cache_margin = \"2m\"\ncache_keepalive_max = \"2m\"",
            "cache_keepalive_max",
            false,
        ),
    ]
    .into_iter()
    .filter_map(|(raw, detail, judges_lifetime)| {
        let err = super::super::parse_core_text(
            std::path::Path::new("config.toml"),
            &format!("[harness]\n{raw}"),
        )
        .err()
        .unwrap_or_else(|| panic!("incompatible margin must refuse load: {raw}"))
        .validation_message();
        (!err.contains(detail)
            || err.contains("settings and defaults only") != judges_lifetime
            || err.contains("own cache writes") != judges_lifetime)
            .then_some((raw, detail, err))
    })
    .collect();
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn prompt_cache_defaults_and_overrides() {
    let kind = crate::ids::AgentKind::new_unchecked;
    let defaults = HarnessConfig::default();
    assert!(defaults.cache_keepalive);
    assert_eq!(
        defaults.configured_lifetime(&kind("claude"), None).ttl,
        Some(Duration::from_secs(3600))
    );
    assert_eq!(
        defaults.configured_lifetime(&kind("codex"), None).ttl,
        Some(Duration::from_secs(1800))
    );
    assert_eq!(
        defaults.configured_lifetime(&kind("amp"), None).ttl,
        Some(Duration::from_secs(3600))
    );
    let config: HarnessConfig = toml::from_str("cache_keepalive = false\n[prompt_cache_ttl]\nclaude = \"5m\"\ncodex = \"off\"\namp = \"10m\"").unwrap();
    assert!(!config.cache_keepalive);
    assert_eq!(
        config.configured_lifetime(&kind("claude"), None).ttl,
        Some(Duration::from_secs(300))
    );
    assert_eq!(config.configured_lifetime(&kind("codex"), None).ttl, None);
    assert_eq!(
        config.configured_lifetime(&kind("amp"), None).ttl,
        Some(Duration::from_secs(600))
    );
    assert_eq!(
        toml::from_str::<HarnessConfig>(&toml::to_string(&config).unwrap()).unwrap(),
        config
    );
}

#[test]
fn prompt_cache_overrides_refuse_unknown_kinds_and_bad_ttls() {
    for (kind, value) in [
        ("typo", "5m"),
        ("claude", "0s"),
        ("claude", "on"),
        ("claude", "nonsense"),
    ] {
        let raw = format!("[prompt_cache_ttl]\n{kind} = {value:?}");
        let err = toml::from_str::<HarnessConfig>(&raw)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("harness.prompt_cache_ttl.{kind}")),
            "{err}"
        );
    }
}

#[test]
fn cache_keepalive_max_defaults_to_six_hours_and_takes_off_or_a_duration() {
    let six_hours = Some(Duration::from_secs(6 * 3600));
    assert_eq!(HarnessConfig::default().cache_keepalive_max, six_hours);
    let absent: HarnessConfig = toml::from_str("").unwrap();
    assert_eq!(absent.cache_keepalive_max, six_hours);
    for (raw, expected) in [("off", None), ("2h", Some(Duration::from_secs(7200)))] {
        let config: HarnessConfig =
            toml::from_str(&format!("cache_keepalive_max = {raw:?}")).unwrap();
        assert_eq!(config.cache_keepalive_max, expected);
        let rendered = toml::to_string(&config).unwrap();
        assert!(
            rendered.contains(&format!("cache_keepalive_max = {raw:?}")),
            "{rendered}"
        );
        assert_eq!(toml::from_str::<HarnessConfig>(&rendered).unwrap(), config);
    }
    for raw in ["0s", "on", "nonsense"] {
        let err = toml::from_str::<HarnessConfig>(&format!("cache_keepalive_max = {raw:?}"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                "harness.cache_keepalive_max must be off or a non-zero duration such as 6h"
            ),
            "{raw}: {err}"
        );
    }
}

#[test]
fn keep_warm_takes_off_or_a_positive_duration() {
    assert_eq!("off".parse::<KeepWarm>(), Ok(KeepWarm::Off));
    assert_eq!(
        "2h".parse::<KeepWarm>(),
        Ok(KeepWarm::For(Duration::from_secs(7200)))
    );
    assert_eq!(KeepWarm::For(Duration::from_secs(7200)).to_string(), "2h");
    for raw in ["true", "on", "0", "0s", "2x", ""] {
        assert_eq!(raw.parse::<KeepWarm>(), Err(KeepWarmParseError), "{raw}");
    }
}

#[test]
fn keep_warm_floor_defaults_to_fifteen_minutes_and_takes_off() {
    assert_eq!(
        HarnessConfig::default().keep_warm_min_ttl,
        Some(Duration::from_secs(900))
    );
    for (raw, expected) in [("off", None), ("5m", Some(Duration::from_secs(300)))] {
        let config: HarnessConfig =
            toml::from_str(&format!("keep_warm_min_ttl = {raw:?}")).unwrap();
        assert_eq!(config.keep_warm_min_ttl, expected, "{raw}");
        assert_eq!(
            toml::from_str::<HarnessConfig>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
    }
    let err = toml::from_str::<HarnessConfig>("keep_warm_min_ttl = \"5x\"")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("harness.keep_warm_min_ttl must be off or a duration such as 15m"),
        "{err}"
    );
}

#[test]
fn smart_compact_deserializes_percent() {
    let config: HarnessConfig =
        toml::from_str("smart_compact = \"70%\"").expect("parse harness config");

    assert_eq!(config.smart_compact, Some(AutoCompact::Percent(70)));
}

#[test]
fn compaction_defaults_apply_when_unset() {
    for config in [HarnessConfig::default(), toml::from_str("").unwrap()] {
        assert_eq!(config.auto_compact, Some(272_000));
        assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(258 * 1_000)));
    }
}

#[test]
fn native_compaction_takes_off_or_a_window_and_round_trips() {
    for (raw, expected, rendered) in [
        ("off", None, "off"),
        ("100k", Some(100_000), "100k"),
        ("272k", Some(272_000), "272k"),
        ("272000", Some(272_000), "272k"),
        ("272001", Some(272_001), "272001"),
        ("1m", Some(1_000_000), "1000k"),
    ] {
        let parsed = toml::from_str::<HarnessConfig>(&format!("auto_compact = {raw:?}"));
        assert!(parsed.is_ok(), "{raw}: {parsed:?}");
        let config = parsed.unwrap();
        assert_eq!(config.auto_compact, expected);
        let encoded = toml::to_string(&config).unwrap();
        assert!(
            encoded.contains(&format!("auto_compact = {rendered:?}")),
            "{encoded}"
        );
        assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
    }
}

#[test]
fn native_compaction_rejects_invalid_windows_with_the_key() {
    for raw in ["99k", "1000001", "70%", "abc"] {
        let error = toml::from_str::<HarnessConfig>(&format!("auto_compact = {raw:?}"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("harness.auto_compact must be off or a token count from 100k through 1M, such as 272k"), "{raw}: {error}");
    }
}

#[test]
fn smart_compaction_takes_off_and_round_trips() {
    let parsed = toml::from_str::<HarnessConfig>("smart_compact = \"off\"");
    assert!(parsed.is_ok(), "{parsed:?}");
    let config = parsed.unwrap();
    assert_eq!(config.smart_compact, None);
    let encoded = toml::to_string(&config).unwrap();
    assert!(encoded.contains("smart_compact = \"off\""), "{encoded}");
    assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
}

#[test]
fn day_cap_requires_day_and_round_trips() {
    let config: HarnessConfig = toml::from_str("budget = \"50.25/day\"").expect("parse day cap");
    assert_eq!(config.budget.map(DayCap::as_usd), Some(50.25));
    let rendered = toml::to_string(&config).expect("serialize harness");
    assert!(rendered.contains("budget = \"50.25/day\""), "{rendered}");
    assert!(
        toml::from_str::<HarnessConfig>("budget = \"50\"")
            .unwrap_err()
            .to_string()
            .contains("must end in `/day`")
    );
}

#[test]
fn turn_cap_requires_plain_amount_and_round_trips() {
    let config: HarnessConfig = toml::from_str("turn_budget = \"$2.50\"").expect("parse turn cap");
    assert_eq!(config.turn_budget.map(TurnCap::as_usd), Some(2.5));
    let rendered = toml::to_string(&config).expect("serialize harness");
    assert!(rendered.contains("turn_budget = \"2.50\""), "{rendered}");
    assert!(
        toml::from_str::<HarnessConfig>("turn_budget = \"3/day\"")
            .unwrap_err()
            .to_string()
            .contains("must be a plain dollar amount")
    );
}

#[test]
fn smart_compact_deserializes_token_count() {
    let config: HarnessConfig =
        toml::from_str("smart_compact = \"120000\"").expect("parse harness config");

    assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(120_000)));
}

#[test]
fn smart_compact_deserializes_suffixed_token_count() {
    let config: HarnessConfig =
        toml::from_str("smart_compact = \"180k\"").expect("parse harness config");

    assert_eq!(config.smart_compact, Some(AutoCompact::Tokens(180_000)));
}

#[test]
fn smart_compact_round_trips() {
    let config = HarnessConfig {
        smart_compact: Some(AutoCompact::Percent(70)),
        ..Default::default()
    };

    let toml = toml::to_string(&config).expect("serialize harness config");
    let back: HarnessConfig = toml::from_str(&toml).expect("parse harness config");

    assert_eq!(back, config);
}

#[test]
fn flip_compact_round_trips() {
    for (raw, expected) in [
        ("off", FlipCompact::Off),
        ("180k", FlipCompact::Threshold(AutoCompact::Tokens(180_000))),
        ("70%", FlipCompact::Threshold(AutoCompact::Percent(70))),
    ] {
        let config: HarnessConfig = toml::from_str(&format!("flip_compact = {raw:?}"))
            .expect("valid flip compaction policy");
        assert_eq!(config.flip_compact, Some(expected));
        let encoded = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), config);
    }
    assert!(toml::from_str::<HarnessConfig>("flip_compact = \"OFF\"").is_err());
}

#[test]
fn compact_instruction_defaults_to_the_brief_for_the_seat() {
    let config: HarnessConfig = toml::from_str("").expect("parse harness config");

    assert_eq!(
        config.compact_instruction(CompactSeat::Solo),
        DEFAULT_COMPACT_INSTRUCTION
    );
    assert_eq!(
        config.compact_instruction(CompactSeat::Team),
        TEAM_COMPACT_INSTRUCTION
    );
    assert!(
        !toml::to_string(&config)
            .expect("serialize harness config")
            .contains("compact_instruction")
    );
}

#[test]
fn compact_instruction_empty_value_round_trips() {
    let config: HarnessConfig =
        toml::from_str("compact_instruction = \"\"").expect("parse harness config");

    assert_eq!(config.compact_instruction, Some(String::new()));
    assert_eq!(config.compact_instruction(CompactSeat::Solo), "");
    assert_eq!(config.compact_instruction(CompactSeat::Team), "");
    let rendered = toml::to_string(&config).expect("serialize harness config");
    assert!(
        rendered.contains("compact_instruction = \"\""),
        "{rendered}"
    );
    assert_eq!(
        toml::from_str::<HarnessConfig>(&rendered).expect("parse rendered harness config"),
        config
    );
}

#[test]
fn idle_compact_defaults_to_100k() {
    let config: HarnessConfig = toml::from_str("").expect("parse harness config");

    assert_eq!(
        serde_json::to_value(config).unwrap()["idle_compact"],
        "100k"
    );
}

#[test]
fn idle_compact_token_floors_round_trip_at_every_toml_site() {
    for (raw, expected) in [
        ("off", "off"),
        ("25", "25"),
        ("2k", "2k"),
        ("100k", "100k"),
        ("250000", "250k"),
        ("1000k", "1000k"),
    ] {
        let harness: HarnessConfig = toml::from_str(&format!("idle_compact = \"{raw}\"")).unwrap();
        let encoded = toml::to_string(&harness).expect("serialize harness config");
        assert_eq!(toml::from_str::<HarnessConfig>(&encoded).unwrap(), harness);
        let profile: crate::config::Profile =
            toml::from_str(&format!("agent = \"claude\"\nidle-compact = \"{raw}\"")).unwrap();
        let role: crate::config::RoleBinding = toml::from_str(&format!(
            "role = \"worker\"\nprofile = \"worker\"\nidle-compact = \"{raw}\""
        ))
        .unwrap();
        for (value, key) in [
            (serde_json::to_value(harness).unwrap(), "idle_compact"),
            (serde_json::to_value(profile).unwrap(), "idle-compact"),
            (serde_json::to_value(role).unwrap(), "idle-compact"),
        ] {
            assert_eq!(value[key], expected, "{raw}: {value}");
        }
    }
}

#[test]
fn idle_compact_rejects_bad_values() {
    for raw in [
        "auto", "always", "on", "2h", "25m", "1m", "59M", "70%", "0", "0k", "25w",
    ] {
        let err =
            toml::from_str::<HarnessConfig>(&format!("idle_compact = \"{raw}\"")).unwrap_err();
        assert!(
                err.to_string()
                    .contains("idle_compact takes off or a positive token floor in digits or k, such as 100k or 1000k"),
                "{raw}: {err}"
            );
    }
}
