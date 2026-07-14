//! Direct Codex OAuth account-usage probe.
//!
//! Codex's app-server is the low-latency realtime account source. This module
//! supplies the credential-file OAuth account-usage probe: read
//! `~/.codex/auth.json` (honoring `CODEX_HOME`), call the ChatGPT usage
//! endpoint, and normalize the response into Rimz's account-window and
//! paid-usage types. It never writes auth files or refreshes tokens.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{OauthUsageResponse, file_mtime_ms, oauth_http_get};
use crate::agents::{AccountUsageSnapshot, ExtraCredits, HttpErrKind, ResetCredits};

use super::app_server::codex_home;

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";

#[derive(Debug, thiserror::Error)]
pub(crate) enum CodexOauthUsageErr {
    #[error("codex OAuth credentials not found")]
    NoCredentials,
    #[error("codex auth file contains only an API key")]
    ApiKeyOnly,
    #[error("reading codex OAuth credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing codex OAuth credentials, config, or usage response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("codex OAuth usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
}

impl crate::agents::credits::OauthReportable for CodexOauthUsageErr {
    /// Whether this failure is worth reporting off-box. Absent or API-key-only
    /// credentials are the normal state for an app-server or logged-out account,
    /// not a fault; provider 401 and 403 responses are settled auth verdicts
    /// rather than Rimz faults. Parse and other HTTP failures are.
    fn should_report(&self) -> bool {
        !matches!(self, Self::NoCredentials | Self::ApiKeyOnly)
            && !matches!(
                self,
                Self::Http {
                    kind: HttpErrKind::Status(401 | 403),
                    ..
                }
            )
    }
}

pub(crate) type Result<T> = std::result::Result<T, CodexOauthUsageErr>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexOauthCredentials {
    access_token: String,
    account_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexAuth {
    #[serde(default, rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: Option<CodexTokens>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexConfig {
    #[serde(default)]
    chatgpt_base_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageWire {
    plan_type: Option<String>,
    rate_limit: RateLimitWire,
    credits: Option<CreditsWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RateLimitWire {
    primary_window: Option<WindowWire>,
    secondary_window: Option<WindowWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WindowWire {
    used_percent: Option<f64>,
    reset_at: Option<i64>,
    limit_window_seconds: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreditsWire {
    balance: Option<Value>,
    has_credits: Option<bool>,
    unlimited: Option<bool>,
    overage_limit_reached: Option<bool>,
}

pub(crate) fn fetch_usage() -> Result<AccountUsageSnapshot> {
    let home = codex_home().ok_or(CodexOauthUsageErr::NoCredentials)?;
    let credentials = load_credentials_from(&home.join("auth.json"))?;
    let base_url = configured_base_url(&home)?;
    let mut snapshot = fetch_usage_with_url(&usage_url(base_url.as_deref()), &credentials)?;
    snapshot.reset_credits =
        fetch_reset_credits(&reset_credits_url(base_url.as_deref()), &credentials).ok();
    Ok(snapshot)
}

pub(crate) fn credentials_stamp() -> Option<u64> {
    file_mtime_ms(&codex_home()?.join("auth.json"))
}

pub(crate) fn account_key() -> Option<String> {
    let home = codex_home()?;
    load_credentials_from(&home.join("auth.json"))
        .ok()?
        .account_id
}

pub(crate) fn fetch_usage_with_token(
    access_token: &str,
    account_id: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    fetch_usage_with_url(
        &usage_url(None),
        &CodexOauthCredentials {
            access_token: access_token.to_owned(),
            account_id: account_id.map(ToOwned::to_owned),
        },
    )
}

pub(crate) fn load_credentials_from(path: &Path) -> Result<CodexOauthCredentials> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CodexOauthUsageErr::NoCredentials);
        }
        Err(err) => return Err(CodexOauthUsageErr::Io(err)),
    };
    parse_credentials(&bytes)
}

pub(crate) fn parse_credentials(bytes: &[u8]) -> Result<CodexOauthCredentials> {
    let auth: CodexAuth = serde_json::from_slice(bytes)?;
    if auth
        .openai_api_key
        .as_deref()
        .is_some_and(|key| !key.is_empty())
        && auth.tokens.is_none()
    {
        return Err(CodexOauthUsageErr::ApiKeyOnly);
    }
    let Some(tokens) = auth.tokens else {
        return Err(CodexOauthUsageErr::NoCredentials);
    };
    let Some(access_token) = tokens.access_token.and_then(non_empty_trimmed) else {
        return Err(CodexOauthUsageErr::NoCredentials);
    };
    Ok(CodexOauthCredentials {
        access_token,
        account_id: tokens.account_id.and_then(non_empty_trimmed),
    })
}

fn configured_base_url(home: &Path) -> std::io::Result<Option<String>> {
    let path = home.join("config.toml");
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let config: CodexConfig = toml::from_str(&text)
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
            Ok(config.chatgpt_base_url.filter(|url| !url.trim().is_empty()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn usage_url(chatgpt_base_url: Option<&str>) -> String {
    endpoint_url(chatgpt_base_url, "usage")
}

pub(crate) fn reset_credits_url(chatgpt_base_url: Option<&str>) -> String {
    endpoint_url(chatgpt_base_url, "rate-limit-reset-credits")
}

fn endpoint_url(chatgpt_base_url: Option<&str>, endpoint: &str) -> String {
    let base = chatgpt_base_url
        .map(str::trim)
        .filter(|base| !base.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/');
    if base.ends_with("/backend-api") || base.contains("/backend-api/") {
        format!("{base}/wham/{endpoint}")
    } else {
        format!("{base}/api/codex/{endpoint}")
    }
}

pub(crate) fn fetch_usage_with_url(
    url: &str,
    credentials: &CodexOauthCredentials,
) -> Result<AccountUsageSnapshot> {
    let body = http_get(url, credentials)?;
    let mut snapshot = parse_usage_response(&body)?;
    snapshot.account_key = credentials.account_id.clone();
    Ok(snapshot)
}

fn http_get(url: &str, credentials: &CodexOauthCredentials) -> Result<String> {
    let mut headers = vec![
        (
            "Authorization",
            format!("Bearer {}", credentials.access_token),
        ),
        ("Accept", "application/json".to_owned()),
    ];
    if let Some(account_id) = &credentials.account_id {
        headers.push(("ChatGPT-Account-Id", account_id.clone()));
    }
    oauth_http_get(url, &headers, "codex: fetching OAuth account usage")
        .map_err(|(kind, host)| CodexOauthUsageErr::Http { kind, host })
}

pub(crate) fn parse_usage_response(body: &str) -> Result<AccountUsageSnapshot> {
    Ok(serde_json::from_str::<UsageWire>(body)?.into_account_usage())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResetCreditsWire {
    credits: Vec<ResetCreditWire>,
    available_count: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResetCreditWire {
    status: Option<String>,
    expires_at: Option<String>,
}

pub(crate) fn parse_reset_credits(body: &str) -> Result<ResetCredits> {
    let wire = serde_json::from_str::<ResetCreditsWire>(body)?;
    let mut available_count = 0u32;
    let mut expiries = Vec::new();
    for credit in wire.credits {
        if credit.status.as_deref() != Some("available") {
            continue;
        }
        available_count = available_count.saturating_add(1);
        if let Some(expiry) = credit
            .expires_at
            .as_deref()
            .and_then(|expires_at| expires_at.parse::<Timestamp>().ok())
        {
            expiries.push(expiry);
        }
    }
    Ok(ResetCredits {
        count: wire.available_count.unwrap_or(available_count),
        soonest_expiry: expiries.into_iter().min(),
    })
}

fn fetch_reset_credits(url: &str, credentials: &CodexOauthCredentials) -> Result<ResetCredits> {
    let body = http_get(url, credentials)?;
    parse_reset_credits(&body)
}

impl OauthUsageResponse for UsageWire {
    fn into_account_usage(self) -> AccountUsageSnapshot {
        AccountUsageSnapshot {
            account_key: None,
            scope: Default::default(),
            rate_limits: collect_windows(
                self.rate_limit.primary_window,
                self.rate_limit.secondary_window,
            ),
            extra_credits: self.credits.and_then(|credits| {
                credits_to_extra(
                    credits.has_credits,
                    credits.unlimited,
                    credits.overage_limit_reached,
                    credits.balance.as_ref().and_then(parse_balance),
                )
            }),
            reset_credits: None,
            plan: self.plan_type.and_then(non_empty_trimmed),
        }
    }
}

fn non_empty_trimmed(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub(in crate::agents::codex) fn credits_to_extra(
    has_credits: Option<bool>,
    unlimited: Option<bool>,
    overage_limit_reached: Option<bool>,
    balance: Option<f64>,
) -> Option<ExtraCredits> {
    if overage_limit_reached == Some(true) {
        return Some(ExtraCredits::known(None, Some(0.0), None));
    }
    if unlimited == Some(true) {
        return Some(ExtraCredits::known(None, None, None));
    }
    if let Some(balance) = balance {
        return Some(ExtraCredits::known(None, Some(balance), None));
    }
    if has_credits == Some(false) {
        return Some(ExtraCredits::Disabled);
    }
    None
}

fn collect_windows(
    primary: Option<WindowWire>,
    secondary: Option<WindowWire>,
) -> Option<AgentRateLimits> {
    let mut windows: Vec<RateLimitWindow> = [primary, secondary]
        .into_iter()
        .flatten()
        .map(|window| RateLimitWindow {
            used_percentage: window.used_percent.map(clamp_pct),
            resets_at: window
                .reset_at
                .and_then(|secs| Timestamp::from_second(secs).ok()),
            duration_mins: window
                .limit_window_seconds
                .and_then(|seconds| u32::try_from(seconds / 60).ok()),
            // Refreshed straight from Codex's usage API: authoritative, with
            // `observed_at` stamped to the fetch instant at merge.
            observed_at: None,
            source: WindowSource::Authoritative,
            ..Default::default()
        })
        .filter(|window| {
            window.used_percentage.is_some()
                || window.resets_at.is_some()
                || window.duration_mins.is_some()
        })
        .collect();
    windows.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
    (!windows.is_empty()).then_some(AgentRateLimits { windows })
}

fn clamp_pct(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

pub(in crate::agents::codex) fn parse_balance(value: &Value) -> Option<f64> {
    match value {
        Value::Number(value) => value.as_f64().filter(|value| value.is_finite()),
        Value::String(value) => value.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
    .map(|value| value.max(0.0))
}

#[cfg(test)]
#[path = "tests/oauth_usage.rs"]
mod tests;
