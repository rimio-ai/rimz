//! Direct Kimi Code quota probe.

use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{file_mtime_ms, oauth_http_get};
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
    #[error("kimi OAuth usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
}

impl crate::agents::credits::OauthReportable for Error {
    fn should_report(&self) -> bool {
        !matches!(self, Self::NoCredentials | Self::Unavailable)
            && !matches!(self, Self::Http { kind, .. } if kind.is_auth_rejected())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Credentials {
    access_token: Option<String>,
}

pub fn fetch() -> Result<AccountUsageSnapshot, Error> {
    let token = load_token(&super::account::credentials_path())?;
    fetch_with(&usage_url(), &token)
}

fn usage_url() -> String {
    let base = std::env::var("KIMI_CODE_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(configured_managed_base);
    match base {
        Some(base) => format!("{}/usages", base.trim().trim_end_matches('/')),
        None => USAGE_URL.to_owned(),
    }
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
    serde_json::from_slice::<Credentials>(&bytes)?
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or(Error::NoCredentials)
}

fn fetch_with(url: &str, token: &str) -> Result<AccountUsageSnapshot, Error> {
    let headers = [("Authorization", format!("Bearer {token}"))];
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

pub(crate) fn parse_response(body: &str) -> Result<AccountUsageSnapshot, Error> {
    let root: Value = serde_json::from_str(body)?;
    let mut rows = Vec::new();
    if let Some(usage) = root.get("usage") {
        rows.push(usage);
    }
    rows.extend(
        root.get("limits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
    );
    let now = Timestamp::now();
    let windows = rows
        .into_iter()
        .filter_map(|row| parse_window(row, now))
        .collect::<Vec<_>>();
    Ok(AccountUsageSnapshot {
        rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
        extra_credits: root.get("boosterWallet").and_then(parse_booster_wallet),
        plan: Some("Code".to_owned()),
        ..AccountUsageSnapshot::default()
    })
}

fn parse_window(row: &Value, now: Timestamp) -> Option<RateLimitWindow> {
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
    let duration_mins = duration_seconds(row, detail)
        .and_then(|seconds| u32::try_from((seconds.max(0.0) / 60.0).round() as u64).ok());
    Some(RateLimitWindow {
        used_percentage,
        resets_at,
        duration_mins,
        observed_at: Some(now),
        source: WindowSource::Authoritative,
        ..Default::default()
    })
}

fn duration_seconds(row: &Value, detail: &Value) -> Option<f64> {
    if let Some(seconds) = detail
        .get("window")
        .or_else(|| row.get("window"))
        .and_then(number)
    {
        return Some(seconds);
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
        "minute" | "minutes" | "m" => 60.0,
        "hour" | "hours" | "h" => 3_600.0,
        "day" | "days" | "d" => 86_400.0,
        _ => 1.0,
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
    let currency = monthly_limit
        .as_ref()
        .and_then(|(_, currency)| currency.as_deref())
        .or_else(|| {
            monthly_used
                .as_ref()
                .and_then(|(_, currency)| currency.as_deref())
        })
        .unwrap_or("USD");
    if !currency.eq_ignore_ascii_case("USD") {
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
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

fn timestamp(value: &Value) -> Option<Timestamp> {
    if let Some(seconds) = value.as_i64() {
        return Timestamp::from_second(seconds).ok();
    }
    value.as_str()?.parse().ok()
}
