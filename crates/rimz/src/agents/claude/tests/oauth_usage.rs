use super::*;

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
    assert_eq!(
        usage.extra_credits,
        Some(ExtraCredits::known(Some(7.25), None, Some(50.0)))
    );

    let usage = parse_usage_response(r#"{ "extra_usage": { "is_enabled": false } }"#).unwrap();
    assert_eq!(usage.extra_credits, Some(ExtraCredits::Disabled));
}

#[test]
fn user_agent_uses_claude_version_when_supplied() {
    assert_eq!(
        claude_code_user_agent(Some(" 2.1.173 ")),
        "claude-code/2.1.173"
    );
}
