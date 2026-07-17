//! Direct Codex OAuth account-usage probe.
//!
//! Codex's app-server is the low-latency realtime account source. This module
//! supplies the credential-file OAuth account-usage probe: read
//! `~/.codex/auth.json` (honoring `CODEX_HOME`), call the ChatGPT usage
//! endpoint, and normalize the response into RimZ's account-window and
//! paid-usage types. It never writes auth files or refreshes tokens.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::agents::account::file_mtime_ms;
use crate::agents::context::WindowSource;
use crate::agents::credits::oauth_http_get;
use crate::agents::payload::non_empty_trimmed;
use crate::agents::{AccountUsageSnapshot, HttpErrKind, ResetCredits};

use super::account::{UsageCredits, UsageWindow, normalize_usage, parse_balance};

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

impl crate::agents::credits::AccountUsageReportable for CodexOauthUsageErr {
    /// Whether this failure is worth reporting off-box. Absent or API-key-only
    /// credentials are the normal state for an app-server or logged-out account,
    /// not a fault; provider 401 and 403 responses are settled auth verdicts
    /// rather than RimZ faults. Parse and other HTTP failures are.
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

impl CodexOauthCredentials {
    pub(crate) fn account_usage_identity(&self) -> crate::agents::AccountUsageIdentity {
        crate::agents::AccountUsageIdentity {
            account_key: self.account_id.clone(),
            ..Default::default()
        }
    }
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

pub(crate) fn probe_usage() -> crate::agents::AccountUsageProbe {
    let Some(home) = codex_home() else {
        return crate::agents::AccountUsageProbe::NoCredentials(Default::default());
    };
    let credentials_stamp = file_mtime_ms(&home.join("auth.json"));
    let credentials = match load_credentials_from(&home.join("auth.json")) {
        Ok(credentials) => credentials,
        Err(err) => {
            return crate::agents::credits::map_account_usage_probe(
                Err(err),
                crate::agents::AccountUsageIdentity {
                    credentials_stamp,
                    ..Default::default()
                },
                "codex",
            );
        }
    };
    let identity = crate::agents::AccountUsageIdentity {
        credentials_stamp,
        ..credentials.account_usage_identity()
    };
    let result = configured_base_url(&home)
        .map_err(CodexOauthUsageErr::Io)
        .and_then(|base_url| {
            let mut snapshot = fetch_usage_with_url(&usage_url(base_url.as_deref()), &credentials)?;
            snapshot.reset_credits =
                fetch_reset_credits(&reset_credits_url(base_url.as_deref()), &credentials).ok();
            Ok(snapshot)
        });
    crate::agents::credits::map_account_usage_probe(result, identity, "codex")
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

pub(crate) fn account_usage_identity() -> crate::agents::AccountUsageIdentity {
    crate::agents::AccountUsageIdentity {
        credentials_stamp: credentials_stamp(),
        account_key: account_key(),
        ..Default::default()
    }
}

pub(crate) fn load_configured_credentials() -> Result<(CodexOauthCredentials, Option<String>)> {
    let home = codex_home().ok_or(CodexOauthUsageErr::NoCredentials)?;
    let credentials = load_credentials_from(&home.join("auth.json"))?;
    let base_url = configured_base_url(&home)?;
    Ok((credentials, base_url))
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
    let auth = super::account::decode_auth(bytes)?;
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
    let Some(access_token) = tokens.access_token.as_deref().and_then(non_empty_trimmed) else {
        return Err(CodexOauthUsageErr::NoCredentials);
    };
    Ok(CodexOauthCredentials {
        access_token,
        account_id: tokens.account_id.as_deref().and_then(non_empty_trimmed),
    })
}

pub(crate) fn configured_base_url(home: &Path) -> std::io::Result<Option<String>> {
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

pub(crate) fn consume_url(chatgpt_base_url: Option<&str>) -> String {
    endpoint_url(chatgpt_base_url, "rate-limit-reset-credits/consume")
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
    parse_usage_response(&body)
}

fn http_get(url: &str, credentials: &CodexOauthCredentials) -> Result<String> {
    let headers = http_headers(credentials);
    oauth_http_get(url, &headers, "codex: fetching OAuth account usage")
        .map_err(|(kind, host)| CodexOauthUsageErr::Http { kind, host })
}

fn http_headers(credentials: &CodexOauthCredentials) -> Vec<(&'static str, String)> {
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
    headers
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
    id: Option<String>,
    status: Option<String>,
    expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResetCreditDetail {
    pub id: Option<String>,
    pub expires_at: Option<Timestamp>,
}

fn parse_reset_credit_response(body: &str) -> Result<(Option<u32>, Vec<ResetCreditDetail>)> {
    let wire = serde_json::from_str::<ResetCreditsWire>(body)?;
    let details = wire
        .credits
        .into_iter()
        .filter(|credit| credit.status.as_deref() == Some("available"))
        .map(|credit| ResetCreditDetail {
            id: credit.id,
            expires_at: credit
                .expires_at
                .as_deref()
                .and_then(|expires_at| expires_at.parse::<Timestamp>().ok()),
        })
        .collect();
    Ok((wire.available_count, details))
}

pub(crate) fn parse_reset_credits(body: &str) -> Result<ResetCredits> {
    let (available_count, details) = parse_reset_credit_response(body)?;
    Ok(summarize_reset_credits(available_count, &details))
}

fn summarize_reset_credits(
    available_count: Option<u32>,
    details: &[ResetCreditDetail],
) -> ResetCredits {
    let fallback_count = details.len().min(u32::MAX as usize) as u32;
    ResetCredits {
        count: available_count.unwrap_or(fallback_count),
        soonest_expiry: details.iter().filter_map(|credit| credit.expires_at).min(),
    }
}

fn fetch_reset_credits(url: &str, credentials: &CodexOauthCredentials) -> Result<ResetCredits> {
    let body = http_get(url, credentials)?;
    parse_reset_credits(&body)
}

pub(crate) fn fetch_reset_credit_state(
    url: &str,
    credentials: &CodexOauthCredentials,
) -> Result<(ResetCredits, Vec<ResetCreditDetail>)> {
    let body = http_get(url, credentials)?;
    let (available_count, details) = parse_reset_credit_response(&body)?;
    let summary = summarize_reset_credits(available_count, &details);
    Ok((summary, details))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConsumeCode {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    #[serde(other)]
    Unknown,
}

impl ConsumeCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::NothingToReset => "nothing_to_reset",
            Self::NoCredit => "no_credit",
            Self::AlreadyRedeemed => "already_redeemed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) struct ConsumeOutcome {
    pub code: ConsumeCode,
    #[serde(default)]
    pub windows_reset: i64,
}

#[derive(Serialize)]
struct ConsumeRequest<'a> {
    redeem_request_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    credit_id: Option<&'a str>,
}

pub(crate) fn consume_reset_credit(
    credentials: &CodexOauthCredentials,
    base_url: Option<&str>,
    redeem_request_id: &str,
    credit_id: Option<&str>,
) -> Result<ConsumeOutcome> {
    let url = consume_url(base_url);
    let headers = http_headers(credentials);
    let body = crate::agents::credits::oauth_http_post_json(
        &url,
        &headers,
        &ConsumeRequest {
            redeem_request_id,
            credit_id,
        },
        "codex: consuming rate-limit reset credit",
    )
    .map_err(|(kind, host)| CodexOauthUsageErr::Http { kind, host })?;
    Ok(serde_json::from_str(&body)?)
}

impl UsageWire {
    fn into_account_usage(self) -> AccountUsageSnapshot {
        let windows = [
            self.rate_limit.primary_window,
            self.rate_limit.secondary_window,
        ]
        .into_iter()
        .flatten()
        .map(|window| UsageWindow {
            used_percentage: window.used_percent,
            resets_at: window
                .reset_at
                .and_then(|seconds| Timestamp::from_second(seconds).ok()),
            duration_mins: window
                .limit_window_seconds
                .and_then(|seconds| u32::try_from(seconds / 60).ok()),
            scope: None,
            source: WindowSource::Authoritative,
        });
        let credits = self.credits.map(|credits| UsageCredits {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            overage_limit_reached: credits.overage_limit_reached,
            balance: credits.balance.as_ref().and_then(parse_balance),
        });
        normalize_usage(self.plan_type, windows, credits)
    }
}

#[cfg(test)]
#[path = "tests/oauth_usage.rs"]
mod tests;
