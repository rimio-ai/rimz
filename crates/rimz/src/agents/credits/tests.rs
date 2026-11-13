use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn reset_credits_serde_keeps_old_caches_compatible_and_adds_expiries() {
    let old: ResetCredits =
        serde_json::from_str(r#"{"count":2,"soonest_expiry":"2026-07-06T06:30:00Z"}"#).unwrap();
    assert!(old.expiries.is_empty());
    assert_eq!(old.effect, RedeemEffect::RestartsWindow);

    let first = "2026-07-06T06:30:00Z".parse::<Timestamp>().unwrap();
    let second = "2026-07-06T12:00:00Z".parse::<Timestamp>().unwrap();
    let populated =
        ResetCredits::normalized(3, vec![second, first, first], RedeemEffect::KeepsSchedule);
    assert_eq!(populated.soonest_expiry, Some(first));
    assert_eq!(populated.expiries, [first, first, second]);
    assert_eq!(
        serde_json::to_value(populated).unwrap(),
        serde_json::json!({
            "count": 3,
            "soonest_expiry": "2026-07-06T06:30:00Z",
            "expiries": [
                "2026-07-06T06:30:00Z",
                "2026-07-06T06:30:00Z",
                "2026-07-06T12:00:00Z"
            ],
            "effect": "keeps_schedule"
        })
    );

    let with_grants: serde_json::Value = serde_json::json!({
        "count": 1,
        "effect": "keeps_schedule",
        "grants": [{"count": 1, "label": "Free limit reset", "paused": true}]
    });
    let old_build: ResetCredits = serde_json::from_value(with_grants).unwrap();
    assert_eq!(
        old_build.grants[0].label.as_deref(),
        Some("Free limit reset")
    );
    let grant = ResetGrant {
        count: 2,
        expiry: Some(first),
        label: Some("Free limit reset".to_owned()),
        clears: vec!["five_hour".to_owned(), "seven_day".to_owned()],
        ..Default::default()
    };
    let labelled = ResetCredits::from_grants(
        vec![
            grant,
            ResetGrant {
                count: 1,
                ..Default::default()
            },
        ],
        RedeemEffect::KeepsSchedule,
    );
    assert_eq!(
        serde_json::to_value(labelled).unwrap(),
        serde_json::json!({
            "count": 3,
            "soonest_expiry": "2026-07-06T06:30:00Z",
            "expiries": ["2026-07-06T06:30:00Z", "2026-07-06T06:30:00Z"],
            "effect": "keeps_schedule",
            "grants": [
                {
                    "count": 2,
                    "expiry": "2026-07-06T06:30:00Z",
                    "label": "Free limit reset",
                    "clears": ["five_hour", "seven_day"]
                },
                {"count": 1}
            ]
        })
    );
    assert_eq!(
        old_build.refusal(true, first),
        Some(RedeemRefusal::NoGrantSelected),
        "a grant cached before the claim facts is never claimable"
    );

    let claimable = ResetGrant {
        count: 1,
        selected: true,
        cooldown_until: Some(first),
        usable_now: true,
        use_requires_limit: true,
        ..Default::default()
    };
    let json = serde_json::to_value(&claimable).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "count": 1,
            "selected": true,
            "cooldown_until": "2026-07-06T06:30:00Z",
            "usable_now": true,
            "use_requires_limit": true
        })
    );
    assert_eq!(
        serde_json::from_value::<ResetGrant>(json).unwrap(),
        claimable
    );
}

fn at(raw: &str) -> Timestamp {
    raw.parse().unwrap()
}

fn dated(count: u32, expiry: &str) -> ResetGrant {
    ResetGrant {
        count,
        expiry: Some(at(expiry)),
        ..Default::default()
    }
}

fn undated(count: u32) -> ResetGrant {
    ResetGrant {
        count,
        ..Default::default()
    }
}

#[test]
fn from_grants_orders_by_expiry_drops_spent_grants_and_derives_the_flat_summary() {
    let early = "2026-07-01T00:00:00Z";
    let late = "2026-07-09T00:00:00Z";
    let named = |label: &str, grant: ResetGrant| ResetGrant {
        label: Some(label.to_owned()),
        ..grant
    };
    let credits = ResetCredits::from_grants(
        vec![
            named("undated", undated(1)),
            named("b", dated(2, late)),
            dated(0, early),
            named("a", dated(1, early)),
            named("c", dated(1, late)),
        ],
        RedeemEffect::KeepsSchedule,
    );
    assert_eq!(credits.count, 5);
    assert_eq!(credits.soonest_expiry, Some(at(early)));
    assert_eq!(credits.expiries, [at(early), at(late), at(late), at(late)]);
    assert_eq!(
        credits
            .grants
            .iter()
            .map(|grant| grant.label.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "undated"]
    );
}

#[test]
fn accounted_lists_every_credit_of_each_record_shape() {
    let early = "2026-07-01T00:00:00Z";
    let late = "2026-07-09T00:00:00Z";
    let flat = |count, expiries: &[&str], soonest: Option<&str>| ResetCredits {
        count,
        soonest_expiry: soonest.map(at),
        expiries: expiries.iter().copied().map(at).collect(),
        effect: RedeemEffect::RestartsWindow,
        grants: Vec::new(),
    };
    let grants = ResetCredits::from_grants(
        vec![dated(1, late), dated(2, early)],
        RedeemEffect::KeepsSchedule,
    );
    let short = ResetCredits {
        count: 5,
        ..grants.clone()
    };
    for (name, credits, expected) in [
        (
            "grants",
            grants.clone(),
            vec![dated(2, early), dated(1, late)],
        ),
        (
            "grants short of the count",
            short,
            vec![dated(2, early), dated(1, late), undated(2)],
        ),
        (
            "flat, unsorted",
            flat(3, &[late, early, early], Some(early)),
            vec![dated(1, early), dated(1, early), dated(1, late)],
        ),
        (
            "flat longer than the count",
            flat(1, &[late, early], Some(early)),
            vec![dated(1, early)],
        ),
        (
            "soonest only",
            flat(4, &[], Some(early)),
            vec![dated(1, early), undated(3)],
        ),
        ("count only", flat(2, &[], None), vec![undated(2)]),
        (
            "flat short of the count",
            flat(3, &[late], Some(late)),
            vec![dated(1, late), undated(2)],
        ),
        ("zero", flat(0, &[early], Some(early)), Vec::new()),
    ] {
        assert_eq!(credits.accounted(), expected, "{name}");
    }
}

fn serve_many(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
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
                    let Some(header_end) =
                        request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    else {
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
                stream.write_all(response.as_bytes()).unwrap();
                String::from_utf8(request).unwrap()
            })
            .collect()
    });
    (format!("http://{address}"), handle)
}

fn response(status: &str, body: &str, extra_headers: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn oauth_request(post: bool, url: &str) -> std::result::Result<String, OAuthHttpErr> {
    if post {
        oauth_http_post_json(url, &[], &serde_json::json!({"request":"quota"}), "test")
    } else {
        oauth_http_get(url, &[], "test")
    }
}

#[test]
fn http_error_transience() {
    assert!(HttpErrKind::Transport.is_transient());
    assert!(HttpErrKind::Body.is_transient());
    assert!(HttpErrKind::Status(503).is_transient());
    assert!(!HttpErrKind::Status(401).is_transient());
    assert!(!HttpErrKind::Status(404).is_transient());
    assert!(!HttpErrKind::Status(429).is_transient());
}

#[test]
fn usage_urls_accept_only_the_official_https_origin_or_loopback() {
    for url in [
        "https://api.anthropic.com/api/oauth/usage",
        "http://127.0.0.1:8080/x",
        "http://[::1]:9/x",
        "http://localhost:8080/x",
    ] {
        assert!(trusted_usage_url(url, "api.anthropic.com"), "{url}");
    }
    for url in [
        "http://api.anthropic.com/api/oauth/usage",
        "https://api.anthropic.com:8443/api/oauth/usage",
        "https://api.anthropic.com.evil.example/api/oauth/usage",
        "https://api.anthropic.com@evil.example/api/oauth/usage",
        "https://evil.example/api/oauth/usage",
        "not a URL",
    ] {
        assert!(!trusted_usage_url(url, "api.anthropic.com"), "{url}");
    }
}

#[test]
fn json_post_sends_exact_method_path_headers_and_body() {
    #[derive(Serialize)]
    struct Body<'a> {
        commodity: &'a str,
    }
    let (origin, server) = serve_many(vec![response("200 OK", "{}", "")]);
    let result = oauth_http_post_json(
        &format!("{origin}/data/api.json?region=cn"),
        &[("Authorization", "Bearer sentinel-secret".to_owned())],
        &Body { commodity: "plan" },
        "test post",
    );
    assert_eq!(result.unwrap(), "{}");
    let request = server.join().unwrap().pop().unwrap();
    assert!(request.starts_with("POST /data/api.json?region=cn HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sentinel-secret")
    );
    assert!(request.ends_with(r#"{"commodity":"plan"}"#));
}

#[test]
fn secret_bearing_transports_refuse_redirects_and_classify_auth() {
    let (origin, server) = serve_many(vec![response(
        "302 Found",
        "sentinel-response-body",
        "Location: https://example.invalid/leak\r\n",
    )]);
    let error = oauth_http_get(
        &format!("{origin}/redirect"),
        &[("Authorization", "Bearer sentinel-secret".to_owned())],
        "test redirect",
    )
    .unwrap_err();
    assert_eq!(error.kind, HttpErrKind::Status(302));
    assert!(!format!("{error:?}").contains("sentinel-secret"));
    assert!(!format!("{error:?}").contains("sentinel-response-body"));
    server.join().unwrap();

    let (origin, server) = serve_many(vec![response(
        "403 Forbidden",
        "sentinel-response-body",
        "",
    )]);
    let error = oauth_http_post_json(
        &format!("{origin}/quota"),
        &[("X-Api-Key", "sentinel-secret".to_owned())],
        &serde_json::json!({"request":"quota"}),
        "test auth",
    )
    .unwrap_err();
    assert!(error.kind.is_auth_rejected());
    assert!(!format!("{error:?}").contains("sentinel-response-body"));
    server.join().unwrap();
}

#[test]
fn retries_only_transient_failures_and_caps_response_bodies() {
    let (origin, server) = serve_many(vec![
        response("500 Internal Server Error", "", ""),
        response("503 Service Unavailable", "", ""),
        response("200 OK", "done", ""),
    ]);
    assert_eq!(
        oauth_http_get(&format!("{origin}/retry"), &[], "test retry").unwrap(),
        "done"
    );
    assert_eq!(server.join().unwrap().len(), 3);

    let oversized = "x".repeat(OAUTH_HTTP_MAX_BYTES as usize + 1);
    let responses = (0..OAUTH_HTTP_ATTEMPTS)
        .map(|_| response("200 OK", &oversized, ""))
        .collect();
    let (origin, server) = serve_many(responses);
    let error = oauth_http_get(&format!("{origin}/large"), &[], "test cap").unwrap_err();
    assert_eq!(error.kind, HttpErrKind::Body);
    assert_eq!(server.join().unwrap().len(), OAUTH_HTTP_ATTEMPTS as usize);
}

#[test]
fn oauth_get_and_post_share_redirect_status_retry_and_body_limits() {
    for post in [false, true] {
        let (origin, server) = serve_many(vec![response(
            "302 Found",
            "redirect",
            "Location: https://example.invalid/leak\r\n",
        )]);
        assert_eq!(
            oauth_request(post, &format!("{origin}/redirect"))
                .unwrap_err()
                .kind,
            HttpErrKind::Status(302)
        );
        assert_eq!(server.join().unwrap().len(), 1);

        let (origin, server) = serve_many(vec![
            response("500 Internal Server Error", "", ""),
            response("503 Service Unavailable", "", ""),
            response("200 OK", "done", ""),
        ]);
        assert_eq!(
            oauth_request(post, &format!("{origin}/retry")).unwrap(),
            "done"
        );
        assert_eq!(server.join().unwrap().len(), 3);

        for (status, code) in [
            ("401 Unauthorized", 401),
            ("403 Forbidden", 403),
            ("429 Too Many Requests", 429),
        ] {
            let (origin, server) = serve_many(vec![response(status, "rejected", "")]);
            assert_eq!(
                oauth_request(post, &format!("{origin}/status"))
                    .unwrap_err()
                    .kind,
                HttpErrKind::Status(code)
            );
            assert_eq!(server.join().unwrap().len(), 1);
        }

        let oversized = "x".repeat(OAUTH_HTTP_MAX_BYTES as usize + 1);
        let responses = (0..OAUTH_HTTP_ATTEMPTS)
            .map(|_| response("200 OK", &oversized, ""))
            .collect();
        let (origin, server) = serve_many(responses);
        assert_eq!(
            oauth_request(post, &format!("{origin}/large"))
                .unwrap_err()
                .kind,
            HttpErrKind::Body
        );
        assert_eq!(server.join().unwrap().len(), OAUTH_HTTP_ATTEMPTS as usize);
    }
}

#[test]
fn extra_credits_wire_preserves_legacy_shapes_and_unlimited() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum LegacyExtraCredits {
        Disabled,
        Known {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            used_usd: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            remaining_usd: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            limit_usd: Option<f64>,
        },
    }

    for (credits, expected) in [
        (ExtraCredits::Disabled, r#""disabled""#),
        (ExtraCredits::known(None, None, None), r#"{"known":{}}"#),
        (
            ExtraCredits::known(Some(12.0), None, None),
            r#"{"known":{"used_usd":12.0}}"#,
        ),
        (
            ExtraCredits::known(None, Some(7.5), None),
            r#"{"known":{"remaining_usd":7.5}}"#,
        ),
        (
            ExtraCredits::known(None, None, Some(50.0)),
            r#"{"known":{"limit_usd":50.0}}"#,
        ),
        (
            ExtraCredits::known(Some(12.0), Some(7.5), Some(50.0)),
            r#"{"known":{"used_usd":12.0,"remaining_usd":7.5,"limit_usd":50.0}}"#,
        ),
        (ExtraCredits::Unlimited, r#"{"known":{"unlimited":true}}"#),
    ] {
        assert_eq!(serde_json::to_string(&credits).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<ExtraCredits>(expected).unwrap(),
            credits
        );
        let legacy: LegacyExtraCredits = serde_json::from_str(expected).unwrap();
        if credits == ExtraCredits::Unlimited {
            assert_eq!(
                legacy,
                LegacyExtraCredits::Known {
                    used_usd: None,
                    remaining_usd: None,
                    limit_usd: None,
                }
            );
        } else {
            assert_eq!(serde_json::to_string(&legacy).unwrap(), expected);
        }
    }
    assert_eq!(
        serde_json::from_str::<ExtraCredits>(
            r#"{"known":{"unlimited":true,"used_usd":12.0,"remaining_usd":0.0,"limit_usd":50.0}}"#
        )
        .unwrap(),
        ExtraCredits::Unlimited,
    );
    assert_eq!(
        serde_json::from_str::<ExtraCredits>(r#"{"known":{"unlimited":false,"limit_usd":50.0}}"#)
            .unwrap(),
        ExtraCredits::known(None, None, Some(50.0)),
    );
}

#[test]
fn unlimited_credits_have_no_finite_balance_or_limit() {
    let credits = ExtraCredits::Unlimited;
    assert!(!credits.is_disabled());
    assert_eq!(credits.remaining_usd_left(), None);
    assert_eq!(credits.remaining_percentage(), None);
    assert_eq!(credits.limit_usd(), None);
    assert_eq!(
        credits.with_limit_if_missing(Some(25.0)),
        ExtraCredits::Unlimited
    );
}

#[test]
fn extra_credits_usable_until_proven_exhausted() {
    assert!(!ExtraCredits::Disabled.is_usable());
    assert!(!ExtraCredits::known(None, Some(0.0), None).is_usable());
    assert!(!ExtraCredits::known(Some(50.0), None, Some(50.0)).is_usable());
    assert!(ExtraCredits::known(Some(12.0), None, Some(50.0)).is_usable());
    assert!(ExtraCredits::known(None, None, None).is_usable());
    assert!(ExtraCredits::Unlimited.is_usable());
}

#[test]
fn extra_credits_remaining_percentage() {
    // Remaining percentage works from either a used or a remaining figure,
    // and is None when neither pins the balance against the limit.
    assert_eq!(
        ExtraCredits::known(Some(12.0), None, Some(50.0)).remaining_percentage(),
        Some(76)
    );
    assert_eq!(
        ExtraCredits::known(None, Some(7.5), Some(30.0)).remaining_percentage(),
        Some(25)
    );
    assert_eq!(
        ExtraCredits::known(Some(12.0), None, None).remaining_percentage(),
        None
    );
}

#[test]
fn extra_credits_remaining_usd_left_prefers_direct_or_derives_from_limit() {
    assert_eq!(
        ExtraCredits::known(Some(12.0), Some(7.5), Some(50.0)).remaining_usd_left(),
        Some(7.5)
    );
    assert_eq!(
        ExtraCredits::known(Some(12.0), None, Some(50.0)).remaining_usd_left(),
        Some(38.0)
    );
    assert_eq!(
        ExtraCredits::known(Some(75.0), None, Some(50.0)).remaining_usd_left(),
        Some(0.0)
    );
    assert_eq!(
        ExtraCredits::known(None, None, None).remaining_usd_left(),
        None
    );
    assert_eq!(ExtraCredits::Disabled.remaining_usd_left(), Some(0.0));
}
