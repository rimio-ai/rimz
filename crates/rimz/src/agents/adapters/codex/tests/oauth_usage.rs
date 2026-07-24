use super::*;
use crate::agents::ExtraCredits;
use crate::agents::credits::AccountUsageReportable;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn serve_once(body: &str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let response_body = body.to_owned();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8(request).unwrap()
    });
    (format!("http://{address}"), handle)
}

#[test]
fn reportable_classifier_treats_unauthorized_as_settled_auth() {
    assert!(!CodexOauthUsageErr::NoCredentials.should_report());
    assert!(!CodexOauthUsageErr::ApiKeyOnly.should_report());
    assert!(
        !CodexOauthUsageErr::Http {
            kind: HttpErrKind::Status(401),
            host: "chatgpt.com".to_owned(),
        }
        .should_report()
    );
    assert!(
        !CodexOauthUsageErr::Http {
            kind: HttpErrKind::Status(403),
            host: "chatgpt.com".to_owned(),
        }
        .should_report()
    );
    assert!(
        CodexOauthUsageErr::Http {
            kind: HttpErrKind::Status(500),
            host: "chatgpt.com".to_owned(),
        }
        .should_report()
    );
    assert!(
        CodexOauthUsageErr::Http {
            kind: HttpErrKind::Transport,
            host: "chatgpt.com".to_owned(),
        }
        .should_report()
    );
}

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

    let credentials = parse_credentials(
        br#"{
            "OPENAI_API_KEY": "sk-123",
            "tokens": {
                "access_token": "ya29-token",
                "account_id": "acc_123"
            }
        }"#,
    )
    .unwrap();
    assert_eq!(credentials.access_token, "ya29-token");
    assert_eq!(credentials.account_id.as_deref(), Some("acc_123"));

    let credentials = parse_credentials(
        br#"{ "tokens": { "access_token": " token ", "account_id": " acc_123 " } }"#,
    )
    .unwrap();
    assert_eq!(credentials.access_token, "token");
    assert_eq!(credentials.account_id.as_deref(), Some("acc_123"));

    assert!(matches!(
        parse_credentials(b"not json"),
        Err(CodexOauthUsageErr::Parse(_))
    ));
}

#[test]
fn configured_base_url_accepts_only_official_or_loopback_hosts() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(configured_base_url(dir.path()).unwrap(), None);

    for base_url in [
        "https://chatgpt.com/backend-api",
        "http://127.0.0.1:1234/backend-api",
    ] {
        std::fs::write(
            dir.path().join("config.toml"),
            format!("chatgpt_base_url = \"{base_url}\"\n"),
        )
        .unwrap();
        assert_eq!(
            configured_base_url(dir.path()).unwrap().as_deref(),
            Some(base_url)
        );
    }

    std::fs::write(
        dir.path().join("config.toml"),
        "chatgpt_base_url = \"https://proxy.invalid/backend-api\"\n",
    )
    .unwrap();
    let error = configured_base_url(dir.path()).unwrap_err();
    assert!(matches!(error, CodexOauthUsageErr::UntrustedBaseUrl { .. }));
    assert!(!error.should_report());
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
    assert_eq!(
        reset_credits_url(None),
        "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits"
    );
    assert_eq!(
        reset_credits_url(Some("http://127.0.0.1:1234/backend-api/")),
        "http://127.0.0.1:1234/backend-api/wham/rate-limit-reset-credits"
    );
    assert_eq!(
        reset_credits_url(Some("https://chatgpt.com")),
        "https://chatgpt.com/api/codex/rate-limit-reset-credits"
    );
    assert_eq!(
        consume_url(None),
        "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits/consume"
    );
    assert_eq!(
        consume_url(Some("https://chatgpt.com")),
        "https://chatgpt.com/api/codex/rate-limit-reset-credits/consume"
    );
}

#[test]
fn reset_credits_parse_available_count_and_soonest_expiry() {
    let credits = parse_reset_credits(
        r#"{
            "available_count": 4,
            "credits": [
                {
                    "status": "available",
                    "expires_at": "2026-07-06T12:00:00Z",
                    "title": "Rate limit reset"
                },
                {
                    "status": "redeemed",
                    "expires_at": "2026-07-05T12:00:00Z"
                },
                {
                    "status": "available",
                    "expires_at": "2026-07-06T06:30:00Z"
                },
                {
                    "status": "available",
                    "expires_at": "2026-07-06T06:30:00Z"
                },
                {
                    "status": "available",
                    "expires_at": "not-a-timestamp"
                }
            ]
        }"#,
    )
    .unwrap();

    assert_eq!(credits.count, 4);
    assert_eq!(
        credits.soonest_expiry,
        Some("2026-07-06T06:30:00Z".parse::<Timestamp>().unwrap())
    );
    assert_eq!(
        credits.expiries,
        [
            "2026-07-06T06:30:00Z".parse::<Timestamp>().unwrap(),
            "2026-07-06T06:30:00Z".parse::<Timestamp>().unwrap(),
            "2026-07-06T12:00:00Z".parse::<Timestamp>().unwrap(),
        ]
    );
}

#[test]
fn reset_credits_falls_back_to_available_entries() {
    let credits = parse_reset_credits(
        r#"{
            "credits": [
                { "status": "available", "expires_at": "2026-07-08T00:00:00Z" },
                { "status": "expired", "expires_at": "2026-07-01T00:00:00Z" },
                { "status": "available", "expires_at": null }
            ]
        }"#,
    )
    .unwrap();

    assert_eq!(credits.count, 2);
    assert_eq!(
        credits.soonest_expiry,
        Some("2026-07-08T00:00:00Z".parse::<Timestamp>().unwrap())
    );
    assert_eq!(
        credits.expiries,
        ["2026-07-08T00:00:00Z".parse::<Timestamp>().unwrap()]
    );
}

#[test]
fn reset_credit_details_keep_available_ids_and_optional_expiries() {
    let (_, details) = parse_reset_credit_response(
        r#"{
            "credits": [
                { "id": "credit_late", "status": "available", "expires_at": "2026-07-08T00:00:00Z" },
                { "id": "credit_used", "status": "redeemed", "expires_at": "2026-07-01T00:00:00Z" },
                { "status": "available", "expires_at": null },
                { "id": "credit_bad", "status": "available", "expires_at": "bad" }
            ]
        }"#,
    )
    .unwrap();

    assert_eq!(
        details,
        vec![
            ResetCreditDetail {
                id: Some("credit_late".to_owned()),
                expires_at: Some("2026-07-08T00:00:00Z".parse().unwrap()),
            },
            ResetCreditDetail {
                id: None,
                expires_at: None,
            },
            ResetCreditDetail {
                id: Some("credit_bad".to_owned()),
                expires_at: None,
            },
        ]
    );
}

#[test]
fn reset_credit_selection_prefers_the_soonest_known_expiry() {
    let details = vec![
        ResetCreditDetail {
            id: Some("undated".to_owned()),
            expires_at: None,
        },
        ResetCreditDetail {
            id: Some("later".to_owned()),
            expires_at: Timestamp::from_second(300).ok(),
        },
        ResetCreditDetail {
            id: Some("earlier".to_owned()),
            expires_at: Timestamp::from_second(200).ok(),
        },
    ];

    assert_eq!(select_reset_credit_id(&details), Some("earlier"));
    assert_eq!(
        select_reset_credit_id(&[ResetCreditDetail {
            id: None,
            expires_at: None,
        }]),
        None
    );
}

#[test]
fn consume_reset_credit_sends_codex_oauth_contract() {
    let (origin, server) = serve_once(r#"{"code":"reset","windows_reset":2}"#);
    let credentials = CodexOauthCredentials {
        access_token: "sentinel-secret".to_owned(),
        account_id: Some("account-123".to_owned()),
    };

    let outcome = consume_reset_credit(
        &credentials,
        Some(&origin),
        "0195-request",
        Some("credit-456"),
    )
    .unwrap();

    assert_eq!(
        outcome,
        ConsumeOutcome {
            code: ConsumeCode::Reset,
            windows_reset: 2,
        }
    );
    let request = server.join().unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(request.starts_with("POST /api/codex/rate-limit-reset-credits/consume HTTP/1.1"));
    assert!(lower.contains("authorization: bearer sentinel-secret\r\n"));
    assert!(lower.contains("chatgpt-account-id: account-123\r\n"));
    assert!(lower.contains("accept: application/json\r\n"));
    assert!(lower.contains("content-type: application/json\r\n"));
    assert!(request.ends_with(r#"{"redeem_request_id":"0195-request","credit_id":"credit-456"}"#));
}

#[test]
fn consume_response_preserves_known_and_unknown_codes() {
    for (body, code, windows_reset) in [
        (
            r#"{"code":"nothing_to_reset"}"#,
            ConsumeCode::NothingToReset,
            0,
        ),
        (
            r#"{"code":"no_credit","windows_reset":0}"#,
            ConsumeCode::NoCredit,
            0,
        ),
        (
            r#"{"code":"already_redeemed","windows_reset":0}"#,
            ConsumeCode::AlreadyRedeemed,
            0,
        ),
        (
            r#"{"code":"future_code","windows_reset":9}"#,
            ConsumeCode::Unknown,
            9,
        ),
    ] {
        let parsed: ConsumeOutcome = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.code, code);
        assert_eq!(parsed.windows_reset, windows_reset);
    }
}

#[test]
fn usage_response_maps_plan_type() {
    let usage = parse_usage_response(
        r#"{
            "plan_type": "pro",
            "rate_limit": {},
            "credits": null
        }"#,
    )
    .unwrap();

    assert_eq!(usage.plan.as_deref(), Some("pro"));
}

#[test]
fn usage_response_ignores_missing_or_empty_plan_type() {
    let missing = parse_usage_response(r#"{ "rate_limit": {}, "credits": null }"#).unwrap();
    let empty = parse_usage_response(
        r#"{
            "plan_type": " ",
            "rate_limit": {},
            "credits": null
        }"#,
    )
    .unwrap();

    assert_eq!(missing.plan, None);
    assert_eq!(empty.plan, None);
}

#[test]
fn reset_credits_parse_empty_or_bad_payloads() {
    assert!(parse_reset_credits(r#"{"credits":[]}"#).is_ok());
    assert!(parse_reset_credits("").is_err());
    assert!(parse_reset_credits("null").is_err());
    assert!(parse_reset_credits("{").is_err());
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

#[test]
fn usage_response_lifts_missing_five_hour_for_dynamic_month() {
    let usage = parse_usage_response(
        r#"{
            "plan_type": " pro ",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 7,
                    "reset_at": 1780186207,
                    "limit_window_seconds": 2628000
                }
            }
        }"#,
    )
    .unwrap();
    assert_eq!(usage.plan.as_deref(), Some("pro"));
    let windows = usage.rate_limits.unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].duration_mins, Some(300));
    assert!(windows[0].lifted);
    assert_eq!(windows[1].duration_mins, Some(43_800));
}
