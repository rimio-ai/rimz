use super::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

fn now() -> Timestamp {
    "2026-10-25T07:00:00Z".parse().unwrap()
}

fn fixture() -> Value {
    json!({
        "private-response-key": "private-response-value",
        "usage": {
            "rolling": {
                "status": "ok", "percent": 42.9,
                "resetsAt": "2026-10-25T11:00:00Z"
            },
            "weekly": {
                "status": "ok", "percent": 0,
                "resetsAt": "2026-10-26T00:00:00Z"
            },
            "monthly": {
                "status": "rate-limited", "percent": 37,
                "resetsAt": "2026-11-25T06:00:00Z"
            }
        }
    })
}

#[test]
fn zen_usage_publishes_percentage_only_authoritative_windows() {
    let response = parse_response(&fixture().to_string(), now());
    assert!(
        response.is_ok(),
        "Zen usage should produce windows: {response:?}"
    );
    let snapshot = response.unwrap();
    assert_eq!(snapshot.plan, None);
    let windows = snapshot.rate_limits.unwrap().windows;
    assert_eq!(
        windows
            .iter()
            .map(|window| (
                window.duration_mins,
                window.used_percentage,
                window.amount,
                window.resets_at,
                window.observed_at,
                window.source,
                window.fixed_period,
            ))
            .collect::<Vec<_>>(),
        [
            (
                Some(300),
                Some(42),
                None,
                Some("2026-10-25T11:00:00Z".parse().unwrap()),
                Some(now()),
                WindowSource::Authoritative,
                false
            ),
            (
                Some(10_080),
                Some(0),
                None,
                Some("2026-10-26T00:00:00Z".parse().unwrap()),
                Some(now()),
                WindowSource::Authoritative,
                true
            ),
            (
                Some(43_200),
                Some(100),
                None,
                Some("2026-11-25T06:00:00Z".parse().unwrap()),
                Some(now()),
                WindowSource::Authoritative,
                true
            ),
        ]
    );
}

#[test]
fn percentages_floor_and_only_refused_requests_are_spent() {
    let outcomes = [
        (0.0, "ok"),
        (42.9, "ok"),
        (99.99, "ok"),
        (100.0, "ok"),
        (150.0, "ok"),
        (-5.0, "ok"),
        (37.0, "rate-limited"),
    ]
    .map(|(percent, status)| {
        let mut body = fixture();
        body["usage"]["rolling"]["percent"] = json!(percent);
        body["usage"]["rolling"]["status"] = json!(status);
        parse_response(&body.to_string(), now())
            .map(|snapshot| {
                let window = &snapshot.rate_limits.unwrap().windows[0];
                (window.used_percentage, window.is_spent())
            })
            .map_err(|error| error.to_string())
    });
    assert_eq!(
        outcomes,
        [
            Ok((Some(0), false)),
            Ok((Some(42), false)),
            Ok((Some(99), false)),
            Ok((Some(99), false)),
            Ok((Some(99), false)),
            Ok((Some(0), false)),
            Ok((Some(100), true)),
        ]
    );
}

#[test]
fn five_hour_idle_and_expiry_are_decided_at_the_read() {
    let post_io = now() + Duration::from_secs(180);
    let outcomes = [
        ("2026-10-25T07:02:59Z", 50, "ok", None, 0),
        ("2026-10-25T07:03:00Z", 50, "ok", None, 0),
        ("2026-10-25T12:00:00Z", 0, "ok", None, 0),
        (
            "2026-10-25T11:57:00Z",
            0,
            "ok",
            Some("2026-10-25T11:57:00Z"),
            0,
        ),
        (
            "2026-10-25T12:00:00Z",
            50,
            "ok",
            Some("2026-10-25T12:00:00Z"),
            50,
        ),
        (
            "2026-10-25T12:00:00Z",
            37,
            "rate-limited",
            Some("2026-10-25T12:00:00Z"),
            100,
        ),
    ]
    .map(
        |(reset, percent, status, expected_reset, expected_percent)| {
            let mut body = fixture();
            body["usage"]["rolling"] = json!({
                "resetsAt": reset, "percent": percent, "status": status
            });
            let actual = parse_response(&body.to_string(), now())
                .map(|mut snapshot| {
                    clear_inactive_five_hour(&mut snapshot, now(), post_io);
                    let window = &snapshot.rate_limits.unwrap().windows[0];
                    (
                        window.used_percentage,
                        window.resets_at,
                        window.remaining_percentage(post_io),
                    )
                })
                .map_err(|error| error.to_string());
            (
                actual,
                Ok((
                    Some(expected_percent),
                    expected_reset.map(|reset| reset.parse().unwrap()),
                    Some(100 - expected_percent),
                )),
            )
        },
    );
    assert_eq!(
        outcomes.clone().map(|(actual, _)| actual),
        outcomes.map(|(_, expected)| expected),
    );
}

#[test]
fn unused_fixed_week_and_billing_month_have_started() {
    let mut body = fixture();
    body["usage"]["weekly"] = json!({
        "status": "ok", "percent": 0, "resetsAt": "2026-11-01T07:00:00Z"
    });
    body["usage"]["monthly"] = json!({
        "status": "ok", "percent": 0, "resetsAt": "2026-11-25T07:00:00Z"
    });
    let outcome = parse_response(&body.to_string(), now())
        .map(|snapshot| {
            snapshot.rate_limits.unwrap().windows[1..]
                .iter()
                .map(|window| {
                    (
                        window.used_percentage,
                        window.resets_at.is_some(),
                        window.not_started(now()),
                    )
                })
                .collect::<Vec<_>>()
        })
        .map_err(|error| error.to_string());
    assert_eq!(outcome, Ok(vec![(Some(0), true, false); 2]));
}

#[test]
fn schema_errors_name_only_trusted_fields_and_problem_classes() {
    let mut cases = vec![
        ("private-response-value".to_owned(), "body: not JSON"),
        (
            r#"["private-response-key","private-response-value"]"#.to_owned(),
            "body: wrong type",
        ),
    ];
    for (pointer, value, expected) in [
        ("/usage", None, "usage: missing"),
        ("/usage/weekly", Some(Value::Null), "usage.weekly: null"),
        ("/usage/rolling", None, "usage.rolling: missing"),
        (
            "/usage/rolling/percent",
            Some(json!("private-response-value")),
            "usage.rolling.percent: wrong type",
        ),
        (
            "/usage/monthly/status",
            Some(json!("private-response-value")),
            "usage.monthly.status: invalid value",
        ),
        (
            "/usage/weekly/resetsAt",
            Some(json!("private-response-value")),
            "usage.weekly.resetsAt: invalid value",
        ),
        (
            "/usage/rolling/resetsAt",
            None,
            "usage.rolling.resetsAt: missing",
        ),
    ] {
        let mut body = fixture();
        if let Some(value) = value {
            *body.pointer_mut(pointer).unwrap() = value;
        } else {
            let (parent, key) = pointer.rsplit_once('/').unwrap();
            body.pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
        }
        cases.push((body.to_string(), expected));
    }
    let outcomes = cases
        .iter()
        .map(|(body, _)| {
            parse_response(body, now()).err().map(|error| {
                let diagnostic = format!("{error} {error:?}");
                (
                    error.to_string(),
                    error.fault(),
                    diagnostic.contains("private-"),
                )
            })
        })
        .collect::<Vec<_>>();
    let expected = cases
        .iter()
        .map(|(_, expected)| {
            Some((
                format!("OpenCode Go usage schema drift at {expected}"),
                AccountUsageFault::Transient,
                false,
            ))
        })
        .collect::<Vec<_>>();
    assert_eq!(outcomes, expected);
}

fn respond(stream: &mut impl Write, status: u16, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

fn fetch_from_server(
    usage_status: u16,
    usage_body: String,
    tier_status: u16,
    tier_body: &str,
    org: Option<&str>,
) -> (Result<AccountUsageSnapshot>, Vec<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/zen/go/v1/usage", listener.local_addr().unwrap());
    let tier_body = tier_body.to_owned();
    let (stop, stopped) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stopped.recv_timeout(Duration::from_millis(5)).is_ok() {
                        return requests;
                    }
                    continue;
                }
                Err(error) => panic!("accept: {error}"),
            };
            // BSD `accept` copies the listener's `O_NONBLOCK`; Linux does not.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                assert_ne!(read, 0, "request headers must be complete");
                request.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            let (status, body) = if request.starts_with("get /zen/go/v1/usage ") {
                (usage_status, &usage_body)
            } else if request.starts_with("get /console/api/go/status ") {
                (tier_status, &tier_body)
            } else {
                (404, &tier_body)
            };
            requests.push(request);
            respond(&mut stream, status, body);
        }
    });
    let result = fetch_usage_with_url(&url, "private-token", org);
    stop.send(()).unwrap();
    (result, server.join().unwrap())
}

#[test]
fn zen_then_console_use_bearer_and_distinct_optional_org_headers() {
    let outcomes = [Some("private-org"), None, Some("")].map(|org| {
        let (result, requests) = fetch_from_server(
            200,
            fixture().to_string(),
            200,
            r#"{"product":" go-plus ","private-tier-key":"private-tier-value"}"#,
            org,
        );
        (
            result.ok().and_then(|snapshot| snapshot.plan),
            requests
                .iter()
                .map(|request| {
                    (
                        request.lines().next().unwrap().to_owned(),
                        request.contains("authorization: bearer private-token\r\n"),
                        request.contains("x-opencode-org-id:"),
                        request.contains("x-org-id:"),
                        request.contains("private-org\r\n"),
                    )
                })
                .collect::<Vec<_>>(),
        )
    });
    let expected = [true, false, false].map(|has_org| {
        (
            Some("go-plus".to_owned()),
            vec![
                (
                    "get /zen/go/v1/usage http/1.1".to_owned(),
                    true,
                    has_org,
                    false,
                    has_org,
                ),
                (
                    "get /console/api/go/status http/1.1".to_owned(),
                    true,
                    false,
                    has_org,
                    has_org,
                ),
            ],
        )
    });
    assert_eq!(outcomes, expected);
}

#[test]
fn failed_tier_reads_are_single_attempt_and_keep_usage() {
    let outcomes = [
        (500, "private-tier-value"),
        (404, "private-tier-value"),
        (200, "private-tier-value"),
        (200, "{}"),
        (200, r#"{"product":""}"#),
        (200, r#"{"product":"  "}"#),
        (200, r#"{"product":42}"#),
    ]
    .map(|(status, body)| {
        let (result, requests) = fetch_from_server(
            200,
            fixture().to_string(),
            status,
            body,
            Some("private-org"),
        );
        (
            result
                .map(|snapshot| (snapshot.plan, snapshot.rate_limits.unwrap().windows.len()))
                .map_err(|error| error.to_string()),
            requests
                .iter()
                .filter(|request| request.starts_with("get /zen/"))
                .count(),
            requests
                .iter()
                .filter(|request| request.starts_with("get /console/"))
                .count(),
        )
    });
    assert_eq!(outcomes, std::array::from_fn(|_| (Ok((None, 3)), 1, 1)));
}

#[test]
fn failed_usage_never_reads_tier_and_classifies_errors_privately() {
    let outcomes = [
        (401, "private-response-value"),
        (403, "private-response-value"),
        (404, "private-response-value"),
        (500, "private-response-value"),
        (200, "private-response-value"),
    ]
    .map(|(status, body)| {
        let (result, requests) = fetch_from_server(
            status,
            body.to_owned(),
            200,
            r#"{"product":"go-plus"}"#,
            Some("private-org"),
        );
        (
            result.err().map(|error| {
                (
                    error.fault(),
                    format!("{error} {error:?}").contains("private-"),
                )
            }),
            requests
                .iter()
                .filter(|request| request.starts_with("get /zen/"))
                .count(),
            requests
                .iter()
                .filter(|request| request.starts_with("get /console/"))
                .count(),
        )
    });
    assert_eq!(
        outcomes,
        [
            (Some((AccountUsageFault::NoCredentials, false)), 1, 0),
            (Some((AccountUsageFault::NoCredentials, false)), 1, 0),
            (Some((AccountUsageFault::Transient, false)), 1, 0),
            (Some((AccountUsageFault::Transient, false)), 3, 0),
            (Some((AccountUsageFault::Transient, false)), 1, 0),
        ]
    );
}

#[test]
fn go_usage_url_override_requires_the_official_origin_or_loopback() {
    let outcomes = [
        "http://127.0.0.1:1234/zen/go/v1/usage",
        "https://opencode.ai/zen/go/v1/usage",
        "https://opencode.ai.evil.test/usage",
        "http://opencode.ai/zen/go/v1/usage",
        "https://opencode.ai:1234/usage",
    ]
    .map(|url| {
        resolve_usage_url(Some(url))
            .map_err(|error| error.fault())
            .is_ok()
    });
    assert_eq!(
        (resolve_usage_url(None).ok().as_deref(), outcomes),
        (
            Some("https://opencode.ai/zen/go/v1/usage"),
            [true, true, false, false, false]
        )
    );
}
