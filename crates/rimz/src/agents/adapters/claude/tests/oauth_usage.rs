use super::*;
use crate::agents::credits::AccountUsageReportable;

#[test]
fn usage_credentials_and_birth_key_share_one_secret_choice() {
    for (refresh, kind, secret) in [
        (Some(" refresh "), "refresh-token", "refresh"),
        (None, "access-token", "access"),
        (Some("   "), "access-token", "access"),
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "claudeAiOauth": {
                "accessToken": " access ",
                "refreshToken": refresh,
                "expiresAt": 4102444800000_i64,
                "scopes": ["user:profile"]
            }
        }))
        .unwrap();
        let expected = account_key(kind, secret);
        assert_eq!(parse_credentials(&bytes).unwrap().account_key, expected);
        assert_eq!(parse_account_key(&bytes).unwrap(), expected);
    }
}

#[test]
fn user_agent_without_a_supplied_version_keeps_the_cli_product_shape() {
    let ua = claude_code_user_agent();
    assert!(ua.starts_with("claude-cli/") && ua.ends_with(" (external, cli)"));
}

#[test]
fn named_login_env_reads_account_credentials_under_the_named_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let named = temp.path().join("named");
    std::fs::create_dir_all(&named).unwrap();
    let home_env = BTreeMap::from([("HOME".to_owned(), home.to_string_lossy().into_owned())]);
    let login_env = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("claude"),
        "work".parse().unwrap(),
        named.clone(),
    )
    .unwrap()
    .env(&home_env);
    let path = named.join(".credentials.json");
    std::fs::write(&path, "{}").unwrap();
    let stamp = file_mtime_ms(&path).unwrap();
    assert_eq!(credentials_stamp(&login_env), Some(stamp));
    assert_eq!(credentials_stamp(&home_env), None);
    assert_eq!(credentials_path(&login_env), Some(path));
}

#[test]
fn reportable_classifier_treats_unauthorized_as_settled_auth() {
    assert!(
        !ClaudeOauthUsageErr::Http {
            kind: HttpErrKind::Status(401),
            host: "api.anthropic.com".to_owned(),
        }
        .should_report()
    );
    assert!(
        !ClaudeOauthUsageErr::Http {
            kind: HttpErrKind::Status(403),
            host: "api.anthropic.com".to_owned(),
        }
        .should_report()
    );
}

#[test]
fn usage_url_override_accepts_only_official_or_loopback_hosts() {
    let default = format!("{DEFAULT_USAGE_URL}?cedar_ember=1");
    assert_eq!(resolve_usage_url(None).unwrap(), default);
    assert_eq!(resolve_usage_url(Some("")).unwrap(), default);
    for url in [
        "https://api.anthropic.com/api/oauth/usage",
        "http://127.0.0.1:8080/api/oauth/usage",
    ] {
        assert_eq!(
            resolve_usage_url(Some(url)).unwrap(),
            format!("{url}?cedar_ember=1")
        );
    }
    assert_eq!(
        resolve_usage_url(Some("http://127.0.0.1:8080/api/oauth/usage?x=1")).unwrap(),
        "http://127.0.0.1:8080/api/oauth/usage?x=1&cedar_ember=1"
    );

    let url = "https://evil.example/private/path";
    let error = resolve_usage_url(Some(url)).unwrap_err();
    assert!(matches!(
        error,
        ClaudeOauthUsageErr::UntrustedUsageUrl { .. }
    ));
    assert!(!error.should_report());
    let display = error.to_string();
    assert!(display.contains("evil.example"));
    assert!(!display.contains("/private/path"));
}

#[test]
fn credentials_parse_token_expiry_and_scope() {
    let credentials = parse_credentials(
        br#"{
            "claudeAiOauth": {
                "accessToken": "tok_123",
                "expiresAt": 4102444800000,
                "scopes": ["user:profile"]
            }
        }"#,
    )
    .unwrap();
    assert_eq!(credentials.access_token, "tok_123");
    assert_eq!(credentials.account_key.len(), 64);

    assert!(matches!(
        parse_credentials(
            br#"{
                "claudeAiOauth": {
                    "accessToken": "tok_123",
                    "expiresAt": 1,
                    "scopes": ["user:profile"]
                }
            }"#,
        ),
        Err(ClaudeOauthUsageErr::TokenExpired)
    ));
    assert!(matches!(
        parse_credentials(
            br#"{
                "claudeAiOauth": {
                    "accessToken": "tok_123",
                    "expiresAt": 4102444800000,
                    "scopes": ["other"]
                }
            }"#,
        ),
        Err(ClaudeOauthUsageErr::MissingScope)
    ));
    assert!(matches!(
        parse_credentials(
            br#"{
                "claudeAiOauth": {
                    "accessToken": "tok_123",
                    "scopes": ["user:profile"]
                }
            }"#,
        ),
        Err(ClaudeOauthUsageErr::TokenExpired)
    ));
}

#[test]
fn account_key_prefers_refresh_token_and_never_contains_credentials() {
    fn credentials(access: &str, refresh: Option<&str>) -> ClaudeOauthCredentials {
        let refresh = refresh
            .map(|token| format!(r#", "refreshToken": "{token}""#))
            .unwrap_or_default();
        parse_credentials(
            format!(
                r#"{{
                    "claudeAiOauth": {{
                        "accessToken": "{access}"{refresh},
                        "expiresAt": 4102444800000,
                        "scopes": ["user:profile"]
                    }}
                }}"#
            )
            .as_bytes(),
        )
        .unwrap()
    }

    let first = credentials("access-one", Some("refresh-one"));
    let rotated = credentials("access-two", Some("refresh-one"));
    let switched = credentials("access-two", Some("refresh-two"));
    let access_only = credentials("access-only", None);

    assert_eq!(first.account_key, rotated.account_key);
    assert_ne!(first.account_key, switched.account_key);
    assert_eq!(
        access_only.account_key,
        account_key("access-token", "access-only")
    );
    for secret in ["access-one", "access-two", "refresh-one", "refresh-two"] {
        assert!(!first.account_key.contains(secret));
        assert!(!switched.account_key.contains(secret));
    }
}

#[test]
fn birth_account_key_ignores_usage_eligibility_and_matches_valid_credentials() {
    let expired = br#"{
        "claudeAiOauth": {
            "accessToken": "expired-access",
            "refreshToken": "stable-refresh",
            "expiresAt": 1,
            "scopes": ["other"]
        }
    }"#;
    assert!(matches!(
        parse_credentials(expired),
        Err(ClaudeOauthUsageErr::MissingScope)
    ));
    let expired_key = parse_account_key(expired).expect("birth key");

    let valid = br#"{
        "claudeAiOauth": {
            "accessToken": "rotated-access",
            "refreshToken": "stable-refresh",
            "expiresAt": 4102444800000,
            "scopes": ["user:profile"]
        }
    }"#;
    let credentials = parse_credentials(valid).expect("usage credentials");
    assert_eq!(expired_key, credentials.account_key);
    assert_eq!(expired_key, parse_account_key(valid).expect("valid key"));
    for secret in ["expired-access", "rotated-access", "stable-refresh"] {
        assert!(!expired_key.contains(secret));
    }
}

#[test]
fn usage_response_maps_windows_and_extra_usage() {
    let usage = parse_usage_response(
        r#"{
            "five_hour": {
                "utilization": 1.0,
                "resets_at": "2026-09-21T14:13:20Z"
            },
            "seven_day": {
                "utilization": 37.0,
                "resets_at": "2026-09-27T09:06:40Z"
            },
            "limits": [
                {"kind": "session", "percent": 1, "resets_at": "2026-09-21T14:13:20Z"},
                {"kind": "weekly_all", "percent": 37, "resets_at": "2026-09-27T09:06:40Z"},
                {
                    "kind": "weekly_scoped", "group": "weekly", "percent": 58,
                    "resets_at": "2026-09-27T09:06:40Z", "severity": "normal", "is_active": true,
                    "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}
                },
                {"kind": "weekly_scoped", "percent": 58, "scope": null},
                {"kind": "weekly_scoped", "percent": 58, "scope": {"model": {"id": null}}},
                {"kind": "weekly_scoped", "percent": 58, "scope": {"model": {"display_name": "  "}}}
            ],
            "extra_usage": {
                "is_enabled": true,
                "used_credits": 725,
                "monthly_limit": 5000
            }
        }"#,
    )
    .unwrap();
    let windows = usage.rate_limits.expect("windows");
    assert_eq!(windows.windows.len(), 3);
    assert_eq!(
        windows.windows[0].duration_mins,
        Some(super::super::account::FIVE_HOUR_MINS)
    );
    assert_eq!(windows.windows[0].used_percentage, Some(1));
    assert_eq!(
        windows.windows[0].resets_at,
        "2026-09-21T14:13:20Z".parse::<Timestamp>().ok()
    );
    assert_eq!(
        windows.windows[1].duration_mins,
        Some(super::super::account::SEVEN_DAY_MINS)
    );
    assert_eq!(windows.windows[1].used_percentage, Some(37));
    let sub_cap = &windows.windows[2];
    assert_eq!(
        sub_cap.key(),
        crate::agents::context::RateLimitWindowKey::Scope("model:fable".to_owned())
    );
    assert_eq!(sub_cap.scope.as_ref().unwrap().label, "Fable");
    assert_eq!(sub_cap.used_percentage, Some(58));
    assert_eq!(sub_cap.duration_mins, Some(10_080));
    assert_eq!(sub_cap.resets_at, windows.windows[1].resets_at);
    assert!(
        windows
            .windows
            .iter()
            .all(|window| window.source.is_authoritative()),
        "the OAuth usage endpoint is the official API — its windows are authoritative"
    );
    assert!(
        windows
            .windows
            .iter()
            .all(|window| window.observed_at.is_none()),
        "the fetch instant is stamped at merge, not in the pure parser"
    );
    assert_eq!(
        usage.extra_credits,
        Some(ExtraCredits::known(Some(7.25), None, Some(50.0)))
    );

    let usage = parse_usage_response(r#"{ "extra_usage": { "is_enabled": false } }"#).unwrap();
    assert_eq!(usage.extra_credits, Some(ExtraCredits::Disabled));
}

#[test]
fn usage_response_tolerates_verified_full_payload_shape() {
    let usage = parse_usage_response(
        r#"{
            "five_hour": {
                "utilization": 12.5,
                "resets_at": "2026-09-21T14:13:20Z",
                "limit_dollars": null,
                "used_dollars": null,
                "remaining_dollars": null
            },
            "seven_day": {
                "utilization": 7,
                "resets_at": "2026-09-27T09:06:40Z",
                "limit_dollars": null,
                "used_dollars": null,
                "remaining_dollars": null
            },
            "extra_usage": {
                "is_enabled": false,
                "monthly_limit": 0,
                "used_credits": 0.0,
                "utilization": 0,
                "currency": "USD",
                "decimal_places": 2,
                "disabled_reason": "admin_disabled",
                "daily": null,
                "weekly": null
            },
            "limits": [],
            "spend": {},
            "member_dashboard_available": false
        }"#,
    )
    .unwrap();

    let windows = usage.rate_limits.expect("windows");
    assert_eq!(windows.windows.len(), 2);
    assert_eq!(
        windows.windows[0].duration_mins,
        Some(super::super::account::FIVE_HOUR_MINS)
    );
    assert_eq!(windows.windows[0].used_percentage, Some(13));
    assert_eq!(
        windows.windows[1].duration_mins,
        Some(super::super::account::SEVEN_DAY_MINS)
    );
    assert_eq!(windows.windows[1].used_percentage, Some(7));
    assert_eq!(usage.extra_credits, Some(ExtraCredits::Disabled));
}

#[test]
fn user_agent_formats_the_claude_cli_product() {
    assert_eq!(
        user_agent(USER_AGENT_FALLBACK_VERSION),
        "claude-cli/2.1.283 (external, cli)"
    );
}

#[test]
fn eligible_limit_reset_fixture_preserves_usage() {
    let usage = parse_usage_response(include_str!("fixtures/limit-resets-eligible.json")).unwrap();
    let reset = usage.reset_credits.expect("settled reset credits");
    let expiry = "2026-10-22T16:00:00Z".parse::<Timestamp>().unwrap();
    assert_eq!(reset.count, 1);
    assert_eq!(reset.soonest_expiry, Some(expiry));
    assert_eq!(reset.expiries, vec![expiry]);
    let windows = usage.rate_limits.unwrap().windows;
    assert_eq!(windows[0].used_percentage, Some(17));
    assert_eq!(windows[1].used_percentage, Some(49));
    assert_eq!(usage.extra_credits, Some(ExtraCredits::Disabled));
}

#[test]
fn successful_usage_settles_missing_ineligible_and_empty_resets() {
    for body in [
        include_str!("fixtures/limit-resets-ineligible.json"),
        "{}",
        r#"{"cedar_ember":{"eligible":true,"grants":[]}}"#,
        r#"{"cedar_ember":{"eligible":false,"grants":[{"resets_left":1}]}}"#,
    ] {
        let reset = parse_usage_response(body)
            .unwrap()
            .reset_credits
            .expect("settled reset credits");
        assert_eq!(reset.count, 0);
        assert_eq!(reset.soonest_expiry, None);
        assert!(reset.expiries.is_empty());
    }
}

#[test]
fn limit_resets_count_paused_grants_and_sort_repeated_expiries() {
    let usage = parse_usage_response(r#"{"cedar_ember":{"eligible":true,"grants":[{"resets_left":2,"ends_at":"2026-10-22T16:00:00Z","paused":true},{"resets_left":1,"ends_at":"2026-10-21T16:00:00Z"}]}}"#).unwrap();
    let reset = usage.reset_credits.expect("settled reset credits");
    let early = "2026-10-21T16:00:00Z".parse::<Timestamp>().unwrap();
    let late = "2026-10-22T16:00:00Z".parse::<Timestamp>().unwrap();
    assert_eq!(reset.count, 3);
    assert_eq!(reset.soonest_expiry, Some(early));
    assert_eq!(reset.expiries, vec![early, late, late]);
}

#[test]
fn limit_resets_without_valid_expiry_still_count() {
    let usage = parse_usage_response(r#"{"cedar_ember":{"eligible":true,"grants":[{"resets_left":1,"ends_at":null},{"resets_left":1,"ends_at":"invalid"},{"resets_left":1}]}}"#).unwrap();
    let reset = usage.reset_credits.expect("settled reset credits");
    assert_eq!(reset.count, 3);
    assert_eq!(reset.soonest_expiry, None);
    assert!(reset.expiries.is_empty());
}

#[test]
fn keychain_skip_follows_the_config_dir_that_config_home_resolves() {
    let env = |value: &str| BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), value.to_owned())]);
    assert!(names_config_dir(&env("/named")));
    assert!(names_config_dir(&env(" , /named")));
    // These resolve credentials under `$HOME/.claude`, so the keychain applies.
    assert!(!names_config_dir(&env("")));
    assert!(!names_config_dir(&env(",")));
    assert!(!names_config_dir(&env(" , ")));
    assert!(!names_config_dir(&BTreeMap::new()));
}
