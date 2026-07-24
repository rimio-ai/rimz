use super::*;
use crate::agents::credits::AccountUsageReportable;

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
    assert_eq!(resolve_usage_url(None).unwrap(), DEFAULT_USAGE_URL);
    assert_eq!(resolve_usage_url(Some("")).unwrap(), DEFAULT_USAGE_URL);
    for url in [
        "https://api.anthropic.com/api/oauth/usage",
        "http://127.0.0.1:8080/api/oauth/usage",
    ] {
        assert_eq!(resolve_usage_url(Some(url)).unwrap(), url);
    }

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
fn usage_response_maps_windows_and_extra_usage() {
    let usage = parse_usage_response(
        r#"{
            "five_hour": {
                "utilization": 1.0,
                "resets_at": "2026-09-21T14:13:20Z"
            },
            "seven_day": {
                "utilization": 88.4,
                "resets_at": "2026-09-27T09:06:40Z"
            },
            "extra_usage": {
                "is_enabled": true,
                "used_credits": 725,
                "monthly_limit": 5000
            }
        }"#,
    )
    .unwrap();
    let windows = usage.rate_limits.expect("windows");
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
    assert_eq!(windows.windows[1].used_percentage, Some(88));
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
fn user_agent_uses_claude_version_when_supplied() {
    assert_eq!(
        claude_code_user_agent(Some(" 2.1.173 ")),
        "claude-code/2.1.173"
    );
}
