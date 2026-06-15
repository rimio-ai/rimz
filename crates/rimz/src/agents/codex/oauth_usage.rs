//! Direct Codex OAuth account-usage probe.
//!
//! Codex's app-server is the preferred account source when it is reachable. This
//! module supplies the credential-file OAuth fallback: read `~/.codex/auth.json`
//! (honoring `CODEX_HOME`), call the ChatGPT usage endpoint, and normalize the
//! response into Rimz's account-window and paid-usage types. It never writes
//! auth files or refreshes tokens.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::{ExtraCredits, HttpErrKind, url_host};

use super::app_server::codex_home;

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const TIMEOUT_SECS: u64 = 5;
const MAX_BYTES: u64 = 512 * 1024;

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

impl CodexOauthUsageErr {
    /// Whether this failure is worth reporting off-box. Absent or API-key-only
    /// credentials are the normal state for an app-server or logged-out account,
    /// not a fault; parse and HTTP failures are.
    pub(crate) fn should_report(&self) -> bool {
        !matches!(self, Self::NoCredentials | Self::ApiKeyOnly)
    }
}

pub(crate) type Result<T> = std::result::Result<T, CodexOauthUsageErr>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexOauthUsage {
    pub(crate) rate_limits: Option<AgentRateLimits>,
    pub(crate) extra_credits: Option<ExtraCredits>,
    pub(crate) plan: Option<String>,
}

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
}

pub(crate) fn fetch_usage() -> Result<CodexOauthUsage> {
    let home = codex_home().ok_or(CodexOauthUsageErr::NoCredentials)?;
    let credentials = load_credentials_from(&home.join("auth.json"))?;
    let base_url = configured_base_url(&home)?;
    let url = usage_url(base_url.as_deref());
    fetch_usage_with_url(&url, &credentials)
}

pub(crate) fn fetch_usage_with_token(
    access_token: &str,
    account_id: Option<&str>,
) -> Result<CodexOauthUsage> {
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
    let Some(access_token) = tokens.access_token.filter(|token| !token.is_empty()) else {
        return Err(CodexOauthUsageErr::NoCredentials);
    };
    Ok(CodexOauthCredentials {
        access_token,
        account_id: tokens.account_id.filter(|id| !id.is_empty()),
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
    let base = chatgpt_base_url
        .map(str::trim)
        .filter(|base| !base.is_empty())
        .unwrap_or(DEFAULT_BASE_URL)
        .trim_end_matches('/');
    if base.ends_with("/backend-api") || base.contains("/backend-api/") {
        format!("{base}/wham/usage")
    } else {
        format!("{base}/api/codex/usage")
    }
}

pub(crate) fn fetch_usage_with_url(
    url: &str,
    credentials: &CodexOauthCredentials,
) -> Result<CodexOauthUsage> {
    let body = http_get(url, credentials)?;
    parse_usage_response(&body)
}

fn http_get(url: &str, credentials: &CodexOauthCredentials) -> Result<String> {
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        host = %url_host(url),
        "codex: fetching OAuth account usage",
    );
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .build()
        .new_agent();
    let mut request = agent
        .get(url)
        .header(
            "Authorization",
            format!("Bearer {}", credentials.access_token),
        )
        .header("Accept", "application/json");
    if let Some(account_id) = &credentials.account_id {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    // ureq surfaces a non-2xx response as `Error::StatusCode` (its default), so a
    // 401/429 must be read here — the `status != 200` branch below only sees the
    // success codes that come back `Ok`.
    let mut response = request.call().map_err(|err| CodexOauthUsageErr::Http {
        kind: match err {
            ureq::Error::StatusCode(code) => HttpErrKind::Status(code),
            _ => HttpErrKind::Transport,
        },
        host: url_host(url).to_owned(),
    })?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(CodexOauthUsageErr::Http {
            kind: HttpErrKind::Status(status),
            host: url_host(url).to_owned(),
        });
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_string()
        .map_err(|_| CodexOauthUsageErr::Http {
            kind: HttpErrKind::Body,
            host: url_host(url).to_owned(),
        })
}

pub(crate) fn parse_usage_response(body: &str) -> Result<CodexOauthUsage> {
    let parsed: UsageWire = serde_json::from_str(body)?;
    Ok(CodexOauthUsage {
        rate_limits: collect_windows(
            parsed.rate_limit.primary_window,
            parsed.rate_limit.secondary_window,
        ),
        extra_credits: parsed
            .credits
            .as_ref()
            .and_then(CreditsWire::balance_usd)
            .map(|balance| ExtraCredits::known(None, Some(balance), None)),
        plan: parsed.plan_type.filter(|plan| !plan.is_empty()),
    })
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

impl CreditsWire {
    fn balance_usd(&self) -> Option<f64> {
        match self.balance.as_ref()? {
            Value::Number(value) => value.as_f64().filter(|value| value.is_finite()),
            Value::String(value) => value.trim().parse::<f64>().ok(),
            _ => None,
        }
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
    }
}

#[cfg(test)]
#[path = "tests/oauth_usage.rs"]
mod tests;
