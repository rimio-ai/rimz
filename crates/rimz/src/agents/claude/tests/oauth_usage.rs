use super::*;
use crate::agents::credits::OauthReportable;

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
        ClaudeOauthUsageErr::Http {
            kind: HttpErrKind::Status(403),
            host: "api.anthropic.com".to_owned(),
        }
        .should_report()
    );
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
        Some(CLAUDE_FIVE_HOUR_MINS)
    );
    assert_eq!(windows.windows[0].used_percentage, Some(1));
    assert_eq!(
        windows.windows[0].resets_at,
        "2026-09-21T14:13:20Z".parse::<Timestamp>().ok()
    );
    assert_eq!(
        windows.windows[1].duration_mins,
        Some(CLAUDE_SEVEN_DAY_MINS)
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
        Some(CLAUDE_FIVE_HOUR_MINS)
    );
    assert_eq!(windows.windows[0].used_percentage, Some(13));
    assert_eq!(
        windows.windows[1].duration_mins,
        Some(CLAUDE_SEVEN_DAY_MINS)
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
