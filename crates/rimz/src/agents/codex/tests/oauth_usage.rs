use super::*;

#[test]
fn credentials_distinguish_api_key_and_oauth_login() {
    assert!(matches!(
        parse_credentials(br#"{ "OPENAI_API_KEY": "sk-123" }"#),
        Err(CodexOauthUsageErr::ApiKeyOnly)
    ));

    let credentials = parse_credentials(
        br#"{
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": "ya29-token",
                "account_id": "acc_123"
            }
        }"#,
    )
    .unwrap();
    assert_eq!(credentials.access_token, "ya29-token");
    assert_eq!(credentials.account_id.as_deref(), Some("acc_123"));
}

#[test]
fn usage_url_respects_backend_api_base_and_codex_api_base() {
    assert_eq!(
        usage_url(None),
        "https://chatgpt.com/backend-api/wham/usage"
    );
    assert_eq!(
        usage_url(Some("http://127.0.0.1:1234/backend-api/")),
        "http://127.0.0.1:1234/backend-api/wham/usage"
    );
    assert_eq!(
        usage_url(Some("https://chatgpt.com")),
        "https://chatgpt.com/api/codex/usage"
    );
}

#[test]
fn usage_response_maps_windows_and_tolerates_bad_credit_balance() {
    let usage = parse_usage_response(
        r#"{
            "rate_limit": {
                "primary_window": {
                    "used_percent": 42.4,
                    "reset_at": 1780092691,
                    "limit_window_seconds": 18000
                },
                "secondary_window": {
                    "used_percent": 7,
                    "reset_at": 1780186207,
                    "limit_window_seconds": 604800
                }
            },
            "credits": { "balance": "18.50" }
        }"#,
    )
    .unwrap();
    let windows = usage.rate_limits.expect("windows");
    assert_eq!(windows.windows[0].duration_mins, Some(300));
    assert_eq!(windows.windows[0].used_percentage, Some(42));
    assert_eq!(
        windows.windows[0].resets_at,
        Timestamp::from_second(1_780_092_691).ok()
    );
    assert_eq!(windows.windows[1].duration_mins, Some(10080));
    assert!(
        windows
            .windows
            .iter()
            .all(|window| window.source.is_authoritative()),
        "Codex usage comes from the official API — its windows are authoritative"
    );
    assert_eq!(
        usage.extra_credits,
        Some(ExtraCredits::known(None, Some(18.5), None))
    );

    let usage = parse_usage_response(
        r#"{
            "rate_limit": {
                "primary_window": {
                    "used_percent": 42,
                    "limit_window_seconds": 18000
                }
            },
            "credits": { "balance": true }
        }"#,
    )
    .unwrap();
    assert_eq!(usage.extra_credits, None);
    assert_eq!(
        usage.rate_limits.unwrap().windows[0].used_percentage,
        Some(42)
    );
}

#[test]
fn usage_response_maps_credit_state_ladder() {
    let usage = parse_usage_response(r#"{ "credits": { "has_credits": false } }"#).unwrap();
    assert_eq!(usage.extra_credits, Some(ExtraCredits::Disabled));

    let usage = parse_usage_response(r#"{ "credits": { "unlimited": true } }"#).unwrap();
    assert_eq!(
        usage.extra_credits,
        Some(ExtraCredits::known(None, None, None))
    );

    let usage =
        parse_usage_response(r#"{ "credits": { "overage_limit_reached": true } }"#).unwrap();
    assert_eq!(
        usage.extra_credits,
        Some(ExtraCredits::known(None, Some(0.0), None))
    );
}

#[test]
fn usage_response_maps_verified_team_payload_with_disabled_credits() {
    let usage = parse_usage_response(
        r#"{
            "user_id": "user-123",
            "account_id": "acct-123",
            "email": "person@example.com",
            "plan_type": "team",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 42.4,
                    "reset_at": 1780092691,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 991
                },
                "secondary_window": {
                    "used_percent": 7,
                    "reset_at": 1780186207,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 86400
                }
            },
            "credits": {
                "has_credits": false,
                "unlimited": false,
                "overage_limit_reached": false,
                "balance": null,
                "approx_local_messages": null,
                "approx_cloud_messages": null
            },
            "spend_control": {},
            "rate_limit_reset_credits": null
        }"#,
    )
    .unwrap();

    let windows = usage.rate_limits.expect("windows");
    assert_eq!(windows.windows.len(), 2);
    assert_eq!(windows.windows[0].duration_mins, Some(300));
    assert_eq!(windows.windows[1].duration_mins, Some(10080));
    assert_eq!(usage.extra_credits, Some(ExtraCredits::Disabled));
}
