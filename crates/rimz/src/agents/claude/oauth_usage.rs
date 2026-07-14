//! Direct Claude OAuth account-usage probe.
//!
//! This is a read-only fallback over Claude Code's local OAuth credentials. It
//! reads `~/.claude/.credentials.json`, calls the provider usage endpoint, and
//! normalizes the response into Rimz's account-window and paid-usage types. It
//! never refreshes or writes credentials; retry/backoff and cache writes live in
//! the CLI helper that calls this module.

use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{OauthUsageResponse, file_mtime_ms, oauth_http_get};
use crate::agents::{AccountUsageSnapshot, ExtraCredits, HttpErrKind, transcript_fs::home_dir};

use super::statusline::{CLAUDE_FIVE_HOUR_MINS, CLAUDE_SEVEN_DAY_MINS, clamp_rate_limit_used_pct};

const DEFAULT_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const URL_ENV: &str = "RIMZ_CLAUDE_OAUTH_USAGE_URL";
const USER_AGENT_FALLBACK_VERSION: &str = "unknown";
const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/claude-oauth-account-key/v1";
#[cfg(target_os = "macos")]
const KEYCHAIN_TIMEOUT: Duration = Duration::from_millis(1_500);

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClaudeOauthUsageErr {
    #[error("claude OAuth credentials not found")]
    NoCredentials,
    #[error("claude OAuth token is expired")]
    TokenExpired,
    #[error("claude OAuth token is missing user:profile scope")]
    MissingScope,
    #[error("reading claude OAuth credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing claude OAuth credentials or usage response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("claude OAuth usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
}

impl crate::agents::credits::OauthReportable for ClaudeOauthUsageErr {
    /// Whether this failure is worth reporting off-box. Absent credentials, an
    /// expired token, and a missing usage scope are the normal state for an
    /// account that does not feed Rimz its usage, not a fault; a provider 401
    /// is the same settled auth verdict. Parse and other HTTP failures are.
    fn should_report(&self) -> bool {
        !matches!(
            self,
            Self::NoCredentials | Self::TokenExpired | Self::MissingScope
        ) && !matches!(
            self,
            Self::Http { kind, .. } if kind.is_auth_rejected()
        )
    }
}

pub(crate) type Result<T> = std::result::Result<T, ClaudeOauthUsageErr>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClaudeOauthCredentials {
    access_token: String,
    account_key: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CredentialsFile {
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ClaudeAiOauth {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
    scopes: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageWire {
    five_hour: Option<WindowWire>,
    seven_day: Option<WindowWire>,
    extra_usage: Option<ExtraUsageWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WindowWire {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExtraUsageWire {
    is_enabled: Option<bool>,
    used_credits: Option<f64>,
    monthly_limit: Option<f64>,
}

pub(crate) fn fetch_usage(cli_version: Option<&str>) -> Result<AccountUsageSnapshot> {
    let credentials = load_credentials()?;
    fetch_usage_with_url(&usage_url(), &credentials, cli_version)
}

pub(crate) fn fetch_usage_with_token(
    access_token: &str,
    cli_version: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    fetch_usage_with_url(
        &usage_url(),
        &ClaudeOauthCredentials {
            access_token: access_token.trim().to_owned(),
            account_key: account_key("access-token", access_token.trim()),
        },
        cli_version,
    )
}

pub(crate) fn load_credentials() -> Result<ClaudeOauthCredentials> {
    let path = credentials_path();
    match std::fs::read(&path) {
        Ok(bytes) => parse_credentials(&bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => load_keychain_credentials(),
        Err(err) => Err(ClaudeOauthUsageErr::Io(err)),
    }
}

fn load_keychain_credentials() -> Result<ClaudeOauthCredentials> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("/usr/bin/security");
        command
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let output = crate::proc::run_bounded_output(&mut command, KEYCHAIN_TIMEOUT)
            .map_err(ClaudeOauthUsageErr::Io)?;
        if output.timed_out || !output.status.success() {
            return Err(ClaudeOauthUsageErr::NoCredentials);
        }
        parse_credentials(&output.stdout)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ClaudeOauthUsageErr::NoCredentials)
    }
}

fn credentials_path() -> PathBuf {
    home_dir().join(".claude").join(".credentials.json")
}

pub(crate) fn credentials_stamp() -> Option<u64> {
    file_mtime_ms(&credentials_path())
}

pub(crate) fn current_account_key() -> Option<String> {
    load_credentials()
        .ok()
        .map(|credentials| credentials.account_key)
}

pub(crate) fn parse_credentials(bytes: &[u8]) -> Result<ClaudeOauthCredentials> {
    let parsed: CredentialsFile = serde_json::from_slice(bytes)?;
    let Some(oauth) = parsed.claude_ai_oauth else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    let Some(access_token) = oauth.access_token.and_then(non_empty_trimmed) else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    let refresh_token = oauth.refresh_token.and_then(non_empty_trimmed);
    let scopes = oauth.scopes.unwrap_or_default();
    if !scopes.iter().any(|scope| scope == "user:profile") {
        return Err(ClaudeOauthUsageErr::MissingScope);
    }
    let Some(expires_at) = oauth.expires_at else {
        return Err(ClaudeOauthUsageErr::TokenExpired);
    };
    if expires_at <= unix_now_ms() as i64 {
        return Err(ClaudeOauthUsageErr::TokenExpired);
    }
    let account_key_source = refresh_token
        .as_deref()
        .map_or(("access-token", access_token.as_str()), |token| {
            ("refresh-token", token)
        });
    let account_key = account_key(account_key_source.0, account_key_source.1);
    Ok(ClaudeOauthCredentials {
        access_token,
        account_key,
    })
}

pub(crate) fn fetch_usage_with_url(
    url: &str,
    credentials: &ClaudeOauthCredentials,
    cli_version: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    let body = http_get(url, &credentials.access_token, cli_version)?;
    let mut snapshot = parse_usage_response(&body)?;
    snapshot.account_key = Some(credentials.account_key.clone());
    Ok(snapshot)
}

fn non_empty_trimmed(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn account_key(secret_kind: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ACCOUNT_KEY_DOMAIN);
    hasher.update([0]);
    hasher.update(secret_kind.as_bytes());
    hasher.update([0]);
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

fn usage_url() -> String {
    std::env::var(URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_USAGE_URL.to_owned())
}

fn http_get(url: &str, token: &str, cli_version: Option<&str>) -> Result<String> {
    let headers = [
        ("Authorization", format!("Bearer {token}")),
        ("Accept", "application/json".to_owned()),
        ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ("User-Agent", claude_code_user_agent(cli_version)),
    ];
    oauth_http_get(url, &headers, "claude: fetching OAuth account usage")
        .map_err(|(kind, host)| ClaudeOauthUsageErr::Http { kind, host })
}

fn claude_code_user_agent(cli_version: Option<&str>) -> String {
    let version = cli_version
        .and_then(normalized_version)
        .or_else(|| crate::agents::version::probe_cli_version("claude"))
        .unwrap_or_else(|| USER_AGENT_FALLBACK_VERSION.to_owned());
    format!("claude-code/{version}")
}

fn normalized_version(version: &str) -> Option<String> {
    let trimmed = version.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_owned())
}

pub(crate) fn parse_usage_response(body: &str) -> Result<AccountUsageSnapshot> {
    Ok(serde_json::from_str::<UsageWire>(body)?.into_account_usage())
}

impl OauthUsageResponse for UsageWire {
    fn into_account_usage(self) -> AccountUsageSnapshot {
        AccountUsageSnapshot {
            account_key: None,
            scope: Default::default(),
            rate_limits: collect_rate_limits(self.five_hour, self.seven_day),
            extra_credits: collect_extra_usage(self.extra_usage),
            reset_credits: None,
            plan: None,
        }
    }
}

fn collect_rate_limits(
    five_hour: Option<WindowWire>,
    seven_day: Option<WindowWire>,
) -> Option<AgentRateLimits> {
    let windows: Vec<RateLimitWindow> = [
        window(five_hour, CLAUDE_FIVE_HOUR_MINS),
        window(seven_day, CLAUDE_SEVEN_DAY_MINS),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!windows.is_empty()).then_some(AgentRateLimits { windows })
}

fn window(field: Option<WindowWire>, duration_mins: u32) -> Option<RateLimitWindow> {
    let field = field?;
    let used_percentage = clamp_rate_limit_used_pct(field.utilization);
    let resets_at = field
        .resets_at
        .as_deref()
        .and_then(|raw| raw.parse::<Timestamp>().ok());
    (used_percentage.is_some() || resets_at.is_some()).then_some(RateLimitWindow {
        used_percentage,
        resets_at,
        duration_mins: Some(duration_mins),
        // The OAuth usage endpoint is the official API, so this reading is
        // authoritative; `observed_at` is stamped to the fetch instant at merge.
        observed_at: None,
        source: WindowSource::Authoritative,
        ..Default::default()
    })
}

fn collect_extra_usage(field: Option<ExtraUsageWire>) -> Option<ExtraCredits> {
    let field = field?;
    if field.is_enabled == Some(false) {
        return Some(ExtraCredits::Disabled);
    }
    Some(ExtraCredits::known(
        cents_to_usd(field.used_credits),
        None,
        cents_to_usd(field.monthly_limit),
    ))
}

fn cents_to_usd(value: Option<f64>) -> Option<f64> {
    value.map(|value| value / 100.0)
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "tests/oauth_usage.rs"]
mod tests;
