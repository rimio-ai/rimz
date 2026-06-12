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
            "plan_type": "pro",
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
    assert_eq!(
        usage.extra_credits,
        Some(ExtraCredits::known(None, Some(18.5), None))
    );
    assert_eq!(usage.plan.as_deref(), Some("pro"));

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
