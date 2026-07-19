//! Direct Kimi Code quota probe.

use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::account::file_mtime_ms;
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::oauth_http_get;
use crate::agents::{AccountUsageSnapshot, ExtraCredits, HttpErrKind};

const USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    fn should_report(&self) -> bool {
        !matches!(self, Self::NoCredentials | Self::Unavailable)
            && !matches!(self, Self::Http { kind, .. } if kind.is_auth_rejected())
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

pub fn probe() -> crate::agents::AccountUsageProbe {
    let identity = crate::agents::AccountUsageIdentity {
        credentials_stamp: credentials_stamp(),
        ..Default::default()
    };
    let result = refuse_managed_base_override().and_then(|()| {
        let token = load_token(&super::account::credentials_path())?;
        fetch_with(USAGE_URL, &token)
    });
    crate::agents::credits::map_account_usage_probe(result, identity, "kimi")
}

fn refuse_managed_base_override() -> Result<(), Error> {
    let base = std::env::var("KIMI_CODE_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(configured_managed_base);
    if base.is_some_and(|base| !official_managed_base(&base)) {
        return Err(Error::Unavailable);
    }
    Ok(())
}

fn official_managed_base(base: &str) -> bool {
    matches!(
        base.trim().trim_end_matches('/'),
        "https://api.kimi.com/coding/v1" | USAGE_URL
    )
}

fn configured_managed_base() -> Option<String> {
    let path = super::install::config_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let root: toml::Table = toml::from_str(&text).ok()?;
    root.get("providers")
        .and_then(toml::Value::as_table)?
        .get("managed:kimi-code")?
        .as_table()?
        .get("base_url")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn credentials_stamp() -> Option<u64> {
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
    let body =
        oauth_http_get(url, &headers, "Kimi OAuth usage fetch").map_err(|(kind, host)| {
            if matches!(kind, HttpErrKind::Status(404)) {
                Error::Unavailable
            } else {
                Error::Http { kind, host }
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

pub(crate) fn parse_response(body: &str) -> Result<AccountUsageSnapshot, Error> {
    let root: Value = serde_json::from_str(body)?;
    let mut rows = Vec::new();
    if let Some(usage) = root.get("usage") {
        rows.push((usage, Some(10_080)));
    }
    rows.extend(
        root.get("limits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|row| (row, None)),
    );
    let now = Timestamp::now();
    let windows = rows
        .into_iter()
        .filter_map(|(row, duration)| parse_window(row, now, duration))
        .collect::<Vec<_>>();
    let extra_credits = root.get("boosterWallet").and_then(parse_booster_wallet);
    let reported_plan = ["plan", "planName"]
        .into_iter()
        .find_map(|key| root.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
        .map(ToOwned::to_owned);
    if windows.is_empty() && extra_credits.is_none() && reported_plan.is_none() {
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
    fixed_duration_mins: Option<u32>,
) -> Option<RateLimitWindow> {
    let detail = row.get("detail").unwrap_or(row);
    let limit = number(detail.get("limit")?)?;
    if !limit.is_finite() || limit <= 0.0 {
        return None;
    }
    let used = detail.get("used").and_then(number).or_else(|| {
        detail
            .get("remaining")
            .and_then(number)
            .map(|remaining| limit - remaining)
    });
    let used_percentage =
        used.map(|used| ((used.max(0.0).min(limit) / limit) * 100.0).round() as u8);
    let resets_at = ["reset_at", "resetAt", "reset_time", "resetTime"]
        .into_iter()
        .find_map(|key| detail.get(key).or_else(|| row.get(key)).and_then(timestamp))
        .or_else(|| {
            ["reset_in", "resetIn", "ttl"]
                .into_iter()
                .find_map(|key| detail.get(key).or_else(|| row.get(key)).and_then(number))
                .and_then(|seconds| {
                    Timestamp::from_second(now.as_second().saturating_add(seconds as i64)).ok()
                })
        });
    let duration_mins = fixed_duration_mins.or_else(|| {
        duration_minutes(row, detail)
            .and_then(|minutes| u32::try_from(minutes.max(0.0).round() as u64).ok())
    });
    Some(RateLimitWindow {
        used_percentage,
        resets_at,
        duration_mins,
        observed_at: Some(now),
        source: WindowSource::Authoritative,
        ..Default::default()
    })
}

fn duration_minutes(row: &Value, detail: &Value) -> Option<f64> {
    if let Some(seconds) = detail
        .get("window")
        .or_else(|| row.get("window"))
        .and_then(number)
    {
        return Some(seconds / 60.0);
    }
    let window = row
        .get("window")
        .or_else(|| detail.get("window"))?
        .as_object()?;
    let duration = window.get("duration").and_then(number)?;
    let unit = window
        .get("timeUnit")
        .and_then(Value::as_str)
        .unwrap_or("seconds")
        .to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "time_unit_second" | "second" | "seconds" | "s" => 1.0 / 60.0,
        "time_unit_minute" | "minute" | "minutes" | "m" => 1.0,
        "time_unit_hour" | "hour" | "hours" | "h" => 60.0,
        "time_unit_day" | "day" | "days" | "d" => 1_440.0,
        _ => return None,
    };
    Some(duration * multiplier)
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
        assert!(official_managed_base("https://api.kimi.com/coding/v1/"));
        assert!(official_managed_base(USAGE_URL));
        assert!(!official_managed_base("https://proxy.invalid/coding/v1"));
        let headers = usage_headers("sentinel-secret");
        assert_eq!(headers[0], ("Accept", "application/json".to_owned()));
        assert_eq!(
            headers[1],
            ("Authorization", "Bearer sentinel-secret".to_owned())
        );
    }

    #[test]
    fn official_minute_enum_and_top_level_week_are_minutes() {
        let snapshot = parse_response(
            &json!({
                "usage":{"limit":100,"used":25},
                "limits":[{
                    "detail":{"limit":100,"used":50},
                    "window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"}
                }]
            })
            .to_string(),
        )
        .unwrap();
        let windows = snapshot.rate_limits.unwrap().windows;
        assert_eq!(windows[0].duration_mins, Some(10_080));
        assert_eq!(windows[1].duration_mins, Some(300));
        assert_eq!(windows[1].used_percentage, Some(50));
    }

    #[test]
    fn unknown_duration_units_do_not_fall_back_to_seconds() {
        let window = parse_window(
            &json!({
                "detail":{"limit":10,"used":1},
                "window":{"duration":300,"timeUnit":"FORTNIGHTS"}
            }),
            Timestamp::now(),
            None,
        )
        .unwrap();
        assert_eq!(window.duration_mins, None);
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
