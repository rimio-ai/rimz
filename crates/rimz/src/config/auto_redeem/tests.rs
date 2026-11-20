use super::*;

fn kind(name: &str) -> AgentKind {
    AgentKind::new_unchecked(name)
}

fn policy(level: RedeemLevel, min_gain_secs: u64, spend_order: SpendOrder) -> RedeemPolicy {
    RedeemPolicy {
        level,
        min_gain: Duration::from_secs(min_gain_secs),
        spend_order,
    }
}

const HOUR: u64 = 3_600;

#[test]
fn every_provider_defaults_to_rescue_with_a_72h_gain_and_credits_first() {
    let config: AutoRedeemConfig = toml::from_str("").unwrap();
    assert_eq!(config, AutoRedeemConfig::default());
    for name in ["codex", "claude"] {
        assert_eq!(
            config.policy(&kind(name)),
            policy(RedeemLevel::Rescue, 72 * HOUR, SpendOrder::CreditsFirst),
            "{name}"
        );
    }
}

#[test]
fn a_provider_subtable_overrides_only_the_keys_it_sets() {
    let table = "level = \"gain\"\nmin_gain = \"4h\"\nspend_order = \"resets_first\"\n";
    for (subtable, expected) in [
        (
            "level = \"paced\"",
            policy(RedeemLevel::Paced, 4 * HOUR, SpendOrder::ResetsFirst),
        ),
        (
            "min_gain = \"1d\"",
            policy(RedeemLevel::Gain, 24 * HOUR, SpendOrder::ResetsFirst),
        ),
        (
            "spend_order = \"credits_first\"",
            policy(RedeemLevel::Gain, 4 * HOUR, SpendOrder::CreditsFirst),
        ),
    ] {
        let config: AutoRedeemConfig =
            toml::from_str(&format!("{table}[codex]\n{subtable}\n")).unwrap();
        assert_eq!(config.policy(&kind("codex")), expected, "{subtable}");
    }
}

#[test]
fn min_gain_parses_supported_units() {
    for (raw, seconds) in [("30s", 30), ("20m", 1200), ("4h", 14_400), ("3d", 259_200)] {
        for text in [
            format!("min_gain = \"{raw}\""),
            format!("[codex]\nmin_gain = \"{raw}\""),
        ] {
            let config: AutoRedeemConfig = toml::from_str(&text).unwrap();
            assert_eq!(
                config.policy(&kind("codex")).min_gain,
                Duration::from_secs(seconds),
                "{text}"
            );
        }
    }
}

#[test]
fn a_bad_value_is_refused_in_the_table_and_in_a_subtable() {
    for (key, raw, names) in [
        ("level", "always", &["rescue", "gain", "paced"][..]),
        (
            "spend_order",
            "typo",
            &["credits_first", "resets_first"][..],
        ),
        ("min_gain", "12", &[][..]),
        ("min_gain", "1w", &[][..]),
        ("min_gain", "", &[][..]),
    ] {
        for text in [
            format!("{key} = \"{raw}\""),
            format!("[codex]\n{key} = \"{raw}\""),
        ] {
            let error = toml::from_str::<AutoRedeemConfig>(&text)
                .expect_err(&text)
                .to_string();
            for name in names {
                assert!(error.contains(name), "{text}: {error}");
            }
        }
    }
}

#[test]
fn claude_runs_paced_as_gain_and_warns_only_where_its_own_subtable_sets_it() {
    let inherited: AutoRedeemConfig = toml::from_str("level = \"paced\"\n").unwrap();
    assert_eq!(inherited.policy(&kind("codex")).level, RedeemLevel::Paced);
    assert_eq!(inherited.policy(&kind("claude")).level, RedeemLevel::Gain);
    assert!(inherited.provider_warnings().is_empty());

    let gain: AutoRedeemConfig = toml::from_str("[claude]\nlevel = \"gain\"\n").unwrap();
    assert_eq!(gain.policy(&kind("claude")).level, RedeemLevel::Gain);
    assert!(gain.provider_warnings().is_empty());

    let own: AutoRedeemConfig =
        toml::from_str("[claude]\nlevel = \"paced\"\nmin_gain = \"1d\"\n").unwrap();
    assert_eq!(
        own.policy(&kind("claude")),
        policy(RedeemLevel::Gain, 24 * HOUR, SpendOrder::CreditsFirst)
    );
    assert_eq!(
        own.provider_warnings(),
        [(
            "harness.auto_redeem.claude.level".to_owned(),
            "`harness.auto_redeem.claude.level = \"paced\"` runs as `gain`: a claude reset \
             credit keeps the window's schedule, so there is nothing to pace"
                .to_owned()
        )]
    );
}

#[test]
fn a_subtable_for_a_provider_that_cannot_redeem_warns_and_is_ignored() {
    let config: AutoRedeemConfig = toml::from_str(
        "[codex]\nlevel = \"paced\"\n[antigravity]\nlevel = \"gain\"\nmin_gain = \"1h\"\n",
    )
    .unwrap();
    let warnings = config.provider_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (setting, message) = &warnings[0];
    assert_eq!(setting, "harness.auto_redeem.antigravity");
    assert_eq!(
        message,
        "`[harness.auto_redeem.antigravity]` is ignored: antigravity has no automatic \
         reset-credit redemption; remove the table"
    );
    assert_eq!(
        config.policy(&kind("antigravity")),
        policy(RedeemLevel::Rescue, 72 * HOUR, SpendOrder::CreditsFirst)
    );
}
