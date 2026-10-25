//! Read-only Console OAuth usage for OpenCode Go subscriptions.

use jiff::Timestamp;
use serde::Deserialize;
use std::num::NonZeroU64;

use crate::agents::AccountUsageSnapshot;
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowAmount, WindowSource};
use crate::agents::credits::{
    AccountUsageFault, AccountUsageReportable, HttpErrKind, oauth_http_get, trusted_usage_url,
};

const DEFAULT_USAGE_URL: &str = "https://opencode.ai/console/api/go/status";
const URL_ENV: &str = "RIMZ_OPENCODE_GO_USAGE_URL";

#[derive(Debug, thiserror::Error)]
pub(in crate::agents) enum GoUsageErr {
    #[error("OpenCode Go subscription unavailable")]
    NoSubscription,
    #[error("OpenCode Go usage URL override refused")]
    UntrustedUsageUrl,
    #[error("OpenCode Go status HTTP {kind}")]
    Http { kind: HttpErrKind },
    #[error("OpenCode Go status schema drift")]
    Schema,
}

impl AccountUsageReportable for GoUsageErr {
    fn fault(&self) -> AccountUsageFault {
        match self {
            Self::NoSubscription | Self::UntrustedUsageUrl => AccountUsageFault::NoCredentials,
            Self::Http { kind } if kind.is_auth_rejected() || *kind == HttpErrKind::Status(404) => {
                AccountUsageFault::NoCredentials
            }
            _ => AccountUsageFault::Transient,
        }
    }
}

type Result<T> = std::result::Result<T, GoUsageErr>;

pub(in crate::agents) fn fetch_usage_with_token(
    access_token: &str,
    org_id: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    let url = resolve_usage_url(std::env::var(URL_ENV).ok().as_deref())?;
    fetch_usage_with_url(&url, access_token.trim(), org_id)
}

fn fetch_usage_with_url(
    url: &str,
    access_token: &str,
    org_id: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    let mut headers = vec![("Authorization", format!("Bearer {access_token}"))];
    if let Some(org_id) = org_id.filter(|id| !id.is_empty()) {
        headers.push(("x-org-id", org_id.to_owned()));
    }
    // Query freshness precedes I/O; expiry is checked after it.
    let observed_at = Timestamp::now();
    let body = oauth_http_get(url, &headers, "OpenCode Go status fetch")
        .map_err(|error| GoUsageErr::Http { kind: error.kind })?;
    parse_response(&body, observed_at, Timestamp::now())
}

fn resolve_usage_url(override_url: Option<&str>) -> Result<String> {
    let candidate = override_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_USAGE_URL);
    if !trusted_usage_url(candidate, "opencode.ai") {
        return Err(GoUsageErr::UntrustedUsageUrl);
    }
    Ok(candidate.to_owned())
}

#[derive(Deserialize)]
struct StatusWire {
    product: Option<String>,
    access: Option<AccessWire>,
}

#[derive(Deserialize)]
struct AccessWire {
    meters: MetersWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetersWire {
    five_hour: Option<MeterWire>,
    week: MeterWire,
    month: MeterWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MeterWire {
    used_micro_cents: String,
    limit_micro_cents: String,
    resets_at: Timestamp,
}

fn parse_response(
    body: &str,
    observed_at: Timestamp,
    now: Timestamp,
) -> Result<AccountUsageSnapshot> {
    let status: StatusWire = serde_json::from_str(body).map_err(|_| GoUsageErr::Schema)?;
    let access = status.access.ok_or(GoUsageErr::NoSubscription)?;
    let product = status
        .product
        .filter(|product| !product.trim().is_empty())
        .ok_or(GoUsageErr::Schema)?;
    Ok(AccountUsageSnapshot {
        plan: Some(product),
        rate_limits: Some(AgentRateLimits {
            windows: vec![
                window(access.meters.five_hour, 300, observed_at, now)?,
                window(Some(access.meters.week), 10_080, observed_at, now)?,
                window(Some(access.meters.month), 43_200, observed_at, now)?,
            ],
        }),
        ..Default::default()
    })
}

fn window(
    meter: Option<MeterWire>,
    duration_mins: u32,
    observed_at: Timestamp,
    now: Timestamp,
) -> Result<RateLimitWindow> {
    let (mut amount, mut resets_at) = match meter {
        Some(meter) => (
            Some(WindowAmount {
                used: meter
                    .used_micro_cents
                    .parse()
                    .map_err(|_| GoUsageErr::Schema)?,
                limit: meter
                    .limit_micro_cents
                    .parse::<NonZeroU64>()
                    .map_err(|_| GoUsageErr::Schema)?
                    .get(),
            }),
            Some(meter.resets_at),
        ),
        None => (None, None),
    };
    if duration_mins == 300 && resets_at.is_some_and(|reset| reset <= now) {
        amount = amount.map(|amount| WindowAmount { used: 0, ..amount });
        resets_at = None;
    }
    Ok(RateLimitWindow {
        used_percentage: Some(amount.map_or(0, WindowAmount::used_percentage)),
        amount,
        resets_at,
        duration_mins: Some(duration_mins),
        observed_at: Some(observed_at),
        source: WindowSource::Authoritative,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::context::{WindowAmount, WindowSource};
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn now() -> Timestamp {
        "2026-10-25T07:00:00Z".parse().unwrap()
    }

    fn fixture() -> Value {
        json!({
            "product": "go",
            "subscriberUserId": "private-user",
            "access": {
                "startsAt": "2026-10-25T06:00:00Z",
                "endsAt": "2026-11-25T06:00:00Z",
                "meters": {
                    "fiveHour": {
                        "startsAt": "2026-10-25T06:00:00Z",
                        "resetsAt": "2026-10-25T11:00:00Z",
                        "limitMicroCents": "1200000000",
                        "usedMicroCents": "1199999999"
                    },
                    "week": {
                        "resetsAt": "2026-10-26T00:00:00Z",
                        "limitMicroCents": "3000000000",
                        "usedMicroCents": "244233"
                    },
                    "month": {
                        "resetsAt": "2026-11-25T06:00:00Z",
                        "limitMicroCents": "6000000000",
                        "usedMicroCents": "6000000001"
                    }
                }
            }
        })
    }

    #[test]
    fn go_status_keeps_product_integer_amounts_and_stable_authoritative_windows() {
        let response = parse_response(&fixture().to_string(), now(), now());
        assert!(
            response.is_ok(),
            "Go status should produce windows: {response:?}"
        );
        let snapshot = response.unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("go"));
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
                ))
                .collect::<Vec<_>>(),
            [
                (
                    Some(300),
                    Some(99),
                    Some(WindowAmount {
                        used: 1_199_999_999,
                        limit: 1_200_000_000
                    }),
                    Some("2026-10-25T11:00:00Z".parse().unwrap()),
                    Some(now()),
                    WindowSource::Authoritative
                ),
                (
                    Some(10_080),
                    Some(0),
                    Some(WindowAmount {
                        used: 244_233,
                        limit: 3_000_000_000
                    }),
                    Some("2026-10-26T00:00:00Z".parse().unwrap()),
                    Some(now()),
                    WindowSource::Authoritative
                ),
                (
                    Some(43_200),
                    Some(100),
                    Some(WindowAmount {
                        used: 6_000_000_001,
                        limit: 6_000_000_000
                    }),
                    Some("2026-11-25T06:00:00Z".parse().unwrap()),
                    Some(now()),
                    WindowSource::Authoritative
                ),
            ]
        );
    }

    #[test]
    fn absent_or_expired_five_hour_is_full_without_a_countdown() {
        let outcomes = [
            None,
            Some("2026-10-25T06:59:59Z"),
            Some("2026-10-25T07:00:00Z"),
        ]
        .map(|reset| {
            let mut body = fixture();
            if let Some(reset) = reset {
                body["access"]["meters"]["fiveHour"]["resetsAt"] = json!(reset);
            } else {
                body["access"]["meters"]
                    .as_object_mut()
                    .unwrap()
                    .remove("fiveHour");
            }
            parse_response(&body.to_string(), now(), now())
                .map(|snapshot| {
                    let window = &snapshot.rate_limits.unwrap().windows[0];
                    (
                        window.duration_mins,
                        window.used_percentage,
                        window.amount,
                        window.resets_at,
                    )
                })
                .map_err(|error| error.fault())
        });
        let full = Some(WindowAmount {
            used: 0,
            limit: 1_200_000_000,
        });
        assert_eq!(
            outcomes,
            [
                Ok((Some(300), Some(0), None, None)),
                Ok((Some(300), Some(0), full, None)),
                Ok((Some(300), Some(0), full, None)),
            ]
        );
    }

    #[test]
    fn invalid_meters_fail_without_exposing_response_values() {
        let mut bodies = vec!["private-body-not-json".to_owned()];
        for (pointer, value) in [
            ("/access/meters/week", Value::Null),
            ("/access/meters/month", Value::Null),
            ("/access/meters/week/usedMicroCents", json!("private-body")),
            ("/access/meters/week/usedMicroCents", json!("-1")),
            ("/access/meters/month/limitMicroCents", json!("0")),
            ("/access/meters/fiveHour/limitMicroCents", json!("-1")),
            ("/access/meters/month/resetsAt", json!("private-body")),
            ("/product", json!("")),
        ] {
            let mut body = fixture();
            *body.pointer_mut(pointer).unwrap() = value;
            bodies.push(body.to_string());
        }
        let faults = bodies
            .iter()
            .map(|body| {
                parse_response(body, now(), now())
                    .map_err(|error| {
                        let diagnostic = format!("{error} {error:?}");
                        assert!(!diagnostic.contains("private-body"));
                        assert!(!diagnostic.contains("private-user"));
                        error.fault()
                    })
                    .err()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            faults,
            vec![Some(AccountUsageFault::Transient); bodies.len()]
        );
    }

    fn serve(
        status: u16,
        body: String,
        count: usize,
    ) -> (
        String,
        Arc<AtomicUsize>,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/console/api/go/status",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = requests.clone();
        let response = format!(
            "HTTP/1.1 {status} response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            (0..count)
                .map(|_| {
                    let (mut stream, _) = listener.accept().unwrap();
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
                    counted.fetch_add(1, Ordering::Relaxed);
                    stream.write_all(response.as_bytes()).unwrap();
                    String::from_utf8(request).unwrap().to_ascii_lowercase()
                })
                .collect()
        });
        (url, requests, handle)
    }

    #[test]
    fn go_get_uses_bearer_and_optional_org_headers() {
        let outcomes = [Some("test-org"), None, Some("")].map(|org| {
            let (url, count, server) = serve(200, fixture().to_string(), 1);
            let result = fetch_usage_with_url(&url, "test-token", org);
            let requests = count.load(Ordering::Relaxed);
            let request = if requests == 1 {
                server.join().unwrap().pop().unwrap()
            } else {
                String::new()
            };
            (
                result.is_ok(),
                requests,
                request.starts_with("get /console/api/go/status http/1.1\r\n"),
                request.contains("authorization: bearer test-token\r\n"),
                request.contains("x-org-id: test-org\r\n"),
                request.contains("x-org-id:"),
            )
        });
        assert_eq!(
            outcomes,
            [
                (true, 1, true, true, true, true),
                (true, 1, true, true, false, false),
                (true, 1, true, true, false, false)
            ]
        );
    }

    #[test]
    fn go_http_classifies_no_subscription_auth_and_retried_failures_privately() {
        let outcomes = [
            (200, "{}", 1),
            (404, "private-body", 1),
            (401, "private-body", 1),
            (403, "private-body", 1),
            (500, "private-body", 3),
        ]
        .map(|(status, body, expected_requests)| {
            let (url, count, server) = serve(status, body.to_owned(), expected_requests);
            let result = fetch_usage_with_url(&url, "private-token", Some("private-org"));
            let fault = result.err().map(|error| {
                let diagnostic = format!("{error} {error:?}");
                for secret in ["private-body", "private-token", "private-org"] {
                    assert!(!diagnostic.contains(secret));
                }
                error.fault()
            });
            let requests = count.load(Ordering::Relaxed);
            if requests == expected_requests {
                server.join().unwrap();
            }
            (fault, requests)
        });
        assert_eq!(
            outcomes,
            [
                (Some(AccountUsageFault::NoCredentials), 1),
                (Some(AccountUsageFault::NoCredentials), 1),
                (Some(AccountUsageFault::NoCredentials), 1),
                (Some(AccountUsageFault::NoCredentials), 1),
                (Some(AccountUsageFault::Transient), 3),
            ]
        );
    }

    #[test]
    fn go_usage_url_override_requires_the_official_origin_or_loopback() {
        let outcomes = [
            "http://127.0.0.1:1234/status",
            "https://opencode.ai/console/api/go/status",
            "https://opencode.ai.evil.test/status",
            "http://opencode.ai/console/api/go/status",
            "https://opencode.ai:1234/status",
        ]
        .map(|url| resolve_usage_url(Some(url)).is_ok());
        assert_eq!(
            (resolve_usage_url(None).ok().as_deref(), outcomes),
            (
                Some("https://opencode.ai/console/api/go/status"),
                [true, true, false, false, false]
            )
        );
    }
}
