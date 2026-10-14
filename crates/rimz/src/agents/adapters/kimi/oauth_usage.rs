//! Direct Kimi Code quota probe.

use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::account::file_mtime_ms;
use crate::agents::context::{
    AgentRateLimits, RateLimitWindow, RateLimitWindowScope, WindowSource,
};
use crate::agents::credits::{AccountUsageFault, oauth_http_get};
use crate::agents::{AccountUsageSnapshot, ExtraCredits, HttpErrKind};

const MAINLAND_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";
const GLOBAL_USAGE_URL: &str = "https://api.kimi.ai/coding/v1/usages";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("kimi OAuth credentials not found")]
    NoCredentials,
    #[error("reading kimi OAuth credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing kimi OAuth credentials or usage response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("kimi OAuth usage unavailable")]
    Unavailable,
    #[error("kimi OAuth usage schema drift ({shape})")]
    Schema { shape: &'static str },
    #[error("kimi OAuth usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
}

impl crate::agents::credits::AccountUsageReportable for Error {
    fn fault(&self) -> AccountUsageFault {
        if matches!(self, Self::NoCredentials | Self::Unavailable)
            || matches!(self, Self::Http { kind, .. } if kind.is_auth_rejected())
        {
            AccountUsageFault::NoCredentials
        } else {
            AccountUsageFault::Transient
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Credentials {
    access_token: Option<String>,
    #[serde(rename = "refresh_token")]
    _refresh_token: Option<String>,
    expires_at: Option<f64>,
}

pub(super) fn probe() -> crate::agents::AccountUsageProbe {
    let identity = crate::agents::AccountUsageIdentity {
        credentials_stamp: credentials_stamp(),
        ..Default::default()
    };
    let result = official_usage_url().and_then(|url| {
        let token = load_token(&super::account::credentials_path())?;
        fetch_with(url, &token)
    });
    crate::agents::credits::map_account_usage_probe(result, identity, "kimi")
}

/// The managed base resolves like Kimi Code's own: `$KIMI_CODE_BASE_URL`, then the
/// provider's configured base, then the mainland default. Only an official region
/// base receives the managed bearer, so a custom endpoint never sees the token.
fn official_usage_url() -> Result<&'static str, Error> {
    let configured = std::env::var("KIMI_CODE_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            super::account::managed_provider()?
                .base_url
                .filter(|value| !value.trim().is_empty())
        });
    match configured {
        None => Ok(MAINLAND_USAGE_URL),
        Some(base) => official_usage_url_for(&base).ok_or(Error::Unavailable),
    }
}

fn official_usage_url_for(base: &str) -> Option<&'static str> {
    match base.trim().trim_end_matches('/') {
        "https://api.kimi.com/coding/v1" => Some(MAINLAND_USAGE_URL),
        "https://api.kimi.ai/coding/v1" => Some(GLOBAL_USAGE_URL),
        _ => None,
    }
}

fn credentials_stamp() -> Option<u64> {
    file_mtime_ms(&super::account::credentials_path())
}

fn load_token(path: &Path) -> Result<String, Error> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NoCredentials);
        }
        Err(error) => return Err(Error::Io(error)),
    };
    let credentials = serde_json::from_slice::<Credentials>(&bytes)?;
    let expires_at = credentials.expires_at.ok_or(Error::NoCredentials)?;
    if !expires_at.is_finite()
        || expires_at <= (Timestamp::now().as_second().saturating_add(60)) as f64
    {
        return Err(Error::NoCredentials);
    }
    credentials
        .access_token
        .filter(|token| !token.trim().is_empty())
        .ok_or(Error::NoCredentials)
}

fn fetch_with(url: &str, token: &str) -> Result<AccountUsageSnapshot, Error> {
    let headers = usage_headers(token);
    let body = oauth_http_get(url, &headers, "Kimi OAuth usage fetch").map_err(|error| {
        if matches!(error.kind, HttpErrKind::Status(404)) {
            Error::Unavailable
        } else {
            Error::Http {
                kind: error.kind,
                host: error.host,
            }
        }
    })?;
    parse_response(&body)
}

fn usage_headers(token: &str) -> [(&'static str, String); 2] {
    [
        ("Accept", "application/json".to_owned()),
        ("Authorization", format!("Bearer {token}")),
    ]
}

fn parse_response(body: &str) -> Result<AccountUsageSnapshot, Error> {
    let root: Value = serde_json::from_str(body)?;
    let usages = root.get("usages").and_then(Value::as_object);
    if usages.is_none() {
        tracing::debug!("Kimi quota response has no usages block");
    }
    let now = Timestamp::now();
    let windows = [
        ("limit_5h", Some(300), None),
        ("limit_7d", Some(10_080), None),
        ("limit_month_total", None, Some(("month_total", "mo"))),
        ("limit_month_code", None, Some(("month_code", "mcd"))),
    ]
    .into_iter()
    .filter_map(|(key, duration, scope)| parse_window(usages?.get(key)?, now, duration, scope))
    .collect::<Vec<_>>();
    let extra_credits = root.get("boosterWallet").and_then(parse_booster_wallet);
    let reported_plan = ["plan", "planName"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
        .map(ToOwned::to_owned);
    if usages.is_none() && extra_credits.is_none() {
        return Err(Error::Schema {
            shape: "no-known-usage-fields",
        });
    }
    Ok(AccountUsageSnapshot {
        rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
        extra_credits,
        plan: reported_plan.or_else(|| Some("Code".to_owned())),
        ..AccountUsageSnapshot::default()
    })
}

fn parse_window(
    row: &Value,
    now: Timestamp,
    duration_mins: Option<u32>,
    scope: Option<(&str, &str)>,
) -> Option<RateLimitWindow> {
    let ratio = number(row.get("used_ratio")?)?;
    Some(RateLimitWindow {
        used_percentage: Some((ratio.clamp(0.0, 1.0) * 100.0).round() as u8),
        resets_at: row.get("reset_time").and_then(timestamp),
        duration_mins,
        scope: scope.map(|(id, label)| RateLimitWindowScope {
            id: id.to_owned(),
            label: label.to_owned(),
        }),
        observed_at: Some(now),
        source: WindowSource::Authoritative,
        ..Default::default()
    })
}

fn parse_booster_wallet(wallet: &Value) -> Option<ExtraCredits> {
    let balance = wallet.get("balance")?;
    if balance.get("type").and_then(Value::as_str) != Some("BOOSTER") {
        return None;
    }
    let total = integer(balance.get("amount")?)?;
    if total <= 0 {
        return None;
    }
    let remaining_usd = balance
        .get("amountLeft")
        .and_then(integer)
        .map(fixed_point_cents)
        .map(|cents| cents as f64 / 100.0);
    let monthly_limit = parse_money(wallet.get("monthlyChargeLimit"));
    let monthly_used = parse_money(wallet.get("monthlyUsed"));
    let currencies = [monthly_limit.as_ref(), monthly_used.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|(_, currency)| currency.as_deref());
    if currencies
        .into_iter()
        .any(|currency| !currency.eq_ignore_ascii_case("USD"))
    {
        return None;
    }
    let enabled = wallet
        .get("monthlyChargeLimitEnabled")
        .and_then(Value::as_bool)
        == Some(true);
    let limit_usd = enabled
        .then(|| {
            monthly_limit
                .as_ref()
                .map(|(cents, _)| *cents as f64 / 100.0)
        })
        .flatten()
        .filter(|limit| *limit > 0.0);
    let used_usd = enabled
        .then(|| {
            monthly_used
                .as_ref()
                .map(|(cents, _)| *cents as f64 / 100.0)
        })
        .flatten();
    Some(ExtraCredits::known(used_usd, remaining_usd, limit_usd))
}

fn parse_money(value: Option<&Value>) -> Option<(i64, Option<String>)> {
    let value = value?;
    let cents = integer(value.get("priceInCents")?)?;
    let currency = value
        .get("currency")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|currency| !currency.is_empty())
        .map(ToOwned::to_owned);
    Some((cents, currency))
}

fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse::<i64>().ok())
}

fn fixed_point_cents(value: i64) -> i64 {
    let cents = value as f64 / 1_000_000.0;
    if cents > 0.0 && cents < 1.0 {
        1
    } else {
        cents.round() as i64
    }
}

fn number(value: &Value) -> Option<f64> {
    let number = value.as_f64().or_else(|| value.as_str()?.parse().ok())?;
    number.is_finite().then_some(number)
}

fn timestamp(value: &Value) -> Option<Timestamp> {
    if let Some(seconds) = value.as_i64() {
        return Timestamp::from_second(seconds).ok();
    }
    value.as_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_usages_map_ratios_durations_and_month_scopes() {
        let snapshot = parse_response(
            &json!({
                "usages": {
                    "limit_5h": {"used_ratio":0.754,"reset_time":"2030-01-01T00:00:00Z"},
                    "limit_7d": {"used_ratio":"1.2","reset_time":"not-a-time"},
                    "limit_month_total": {"used_ratio":-0.1},
                    "limit_month_code": {"used_ratio":"0.345"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let windows = snapshot.rate_limits.unwrap().windows;
        assert_eq!(windows.len(), 4);
        assert_eq!(
            windows
                .iter()
                .map(|w| w.used_percentage)
                .collect::<Vec<_>>(),
            vec![Some(75), Some(100), Some(0), Some(35)]
        );
        assert_eq!(
            windows.iter().map(|w| w.duration_mins).collect::<Vec<_>>(),
            vec![Some(300), Some(10_080), None, None]
        );
        assert_eq!(
            windows[0].resets_at,
            Some("2030-01-01T00:00:00Z".parse().unwrap())
        );
        assert_eq!(windows[1].resets_at, None);
        assert_eq!(windows[2].scope.as_ref().unwrap().id, "month_total");
        assert_eq!(windows[3].scope.as_ref().unwrap().id, "month_code");
        assert!(
            windows[2..]
                .iter()
                .all(|w| w.scope.as_ref().unwrap().label.chars().count() <= 3)
        );
    }

    #[test]
    fn token_requires_more_than_sixty_seconds_of_freshness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        for (offset, accepted) in [(3_600, true), (60, false), (-1, false)] {
            std::fs::write(
                &path,
                json!({
                    "access_token":"sentinel-secret",
                    "refresh_token":"refresh-secret",
                    "expires_at":Timestamp::now().as_second() + offset
                })
                .to_string(),
            )
            .unwrap();
            let result = load_token(&path);
            assert_eq!(result.is_ok(), accepted);
            if let Err(error) = result {
                assert!(!error.to_string().contains("sentinel-secret"));
                assert!(!error.to_string().contains("refresh-secret"));
            }
        }
    }

    #[test]
    fn managed_oauth_host_and_headers_are_fixed() {
        assert_eq!(
            official_usage_url_for("https://api.kimi.com/coding/v1/"),
            Some(MAINLAND_USAGE_URL)
        );
        assert_eq!(
            official_usage_url_for("https://api.kimi.ai/coding/v1"),
            Some(GLOBAL_USAGE_URL)
        );
        assert_eq!(official_usage_url_for(MAINLAND_USAGE_URL), None);
        assert_eq!(
            official_usage_url_for("https://proxy.invalid/coding/v1"),
            None
        );
        let headers = usage_headers("sentinel-secret");
        assert_eq!(headers[0], ("Accept", "application/json".to_owned()));
        assert_eq!(
            headers[1],
            ("Authorization", "Bearer sentinel-secret".to_owned())
        );
    }

    #[test]
    fn quota_usages_drop_absent_and_invalid_ratios() {
        for ratio in [
            Value::Null,
            json!("NaN"),
            json!("infinity"),
            json!(""),
            json!({}),
        ] {
            let snapshot = parse_response(
                &json!({"usages":{
                    "limit_5h":{"used_ratio":ratio},
                    "limit_7d":{"reset_time":"2030-01-01T00:00:00Z"},
                    "limit_month_code":{"used_ratio":0.5}
                }})
                .to_string(),
            )
            .unwrap();
            let windows = snapshot.rate_limits.unwrap().windows;
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].scope.as_ref().unwrap().id, "month_code");
        }
    }

    #[test]
    fn quota_rejects_legacy_only_and_keeps_booster_only() {
        assert!(matches!(
            parse_response("{\"usage\":{\"limit\":100,\"used\":25},\"limits\":[]}"),
            Err(Error::Schema { .. })
        ));
        assert!(matches!(
            parse_response(r#"{"plan":"Code"}"#),
            Err(Error::Schema { .. })
        ));
        let snapshot = parse_response(
            &json!({"boosterWallet":{
                "balance":{"type":"BOOSTER","amount":500000000,"amountLeft":125000000},
                "monthlyChargeLimitEnabled":true,
                "monthlyChargeLimit":{"priceInCents":500,"currency":"USD"},
                "monthlyUsed":{"priceInCents":125,"currency":"USD"}
            }})
            .to_string(),
        )
        .unwrap();
        assert!(snapshot.rate_limits.is_none());
        assert_eq!(
            snapshot.extra_credits,
            Some(ExtraCredits::known(Some(1.25), Some(1.25), Some(5.0)))
        );
        assert_eq!(snapshot.plan.as_deref(), Some("Code"));
    }

    #[test]
    fn non_usd_booster_and_empty_success_are_rejected() {
        let body = json!({
            "boosterWallet":{
                "balance":{"type":"BOOSTER","amount":1,"amountLeft":1},
                "monthlyChargeLimitEnabled":true,
                "monthlyChargeLimit":{"priceInCents":100,"currency":"EUR"}
            }
        });
        assert!(matches!(
            parse_response(&body.to_string()),
            Err(Error::Schema { .. })
        ));
        assert!(matches!(parse_response("{}"), Err(Error::Schema { .. })));
    }
}
