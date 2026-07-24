//! Direct Claude OAuth account-usage probe.
//!
//! This is a read-only fallback over Claude Code's local OAuth credentials. It
//! reads `~/.claude/.credentials.json`, calls the provider usage endpoint, and
//! normalizes the response into RimZ's account-window and paid-usage types. It
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

use crate::agents::account::file_mtime_ms;
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{oauth_http_get, trusted_usage_url, url_host};
use crate::agents::payload::non_empty_trimmed;
use crate::agents::{AccountUsageSnapshot, ExtraCredits, HttpErrKind, transcript_fs::home_dir};

const DEFAULT_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OFFICIAL_HOST: &str = "api.anthropic.com";
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
    #[error("claude OAuth usage URL override refused (host {host})")]
    UntrustedUsageUrl { host: String },
    #[error("claude OAuth usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
}

impl crate::agents::credits::AccountUsageReportable for ClaudeOauthUsageErr {
    /// Whether this failure is worth reporting off-box. Absent credentials, an
    /// expired token, a missing usage scope, and a locally refused URL are
    /// settled states, not faults; a provider 401 is the same settled auth
    /// verdict. Parse and other HTTP failures are.
    fn should_report(&self) -> bool {
        !matches!(
            self,
            Self::NoCredentials
                | Self::TokenExpired
                | Self::MissingScope
                | Self::UntrustedUsageUrl { .. }
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

pub(crate) fn probe_usage(cli_version: Option<&str>) -> crate::agents::AccountUsageProbe {
    let credentials_stamp = credentials_stamp();
    let url = match usage_url() {
        Ok(url) => url,
        Err(err) => {
            return crate::agents::credits::map_account_usage_probe(
                Err(err),
                crate::agents::AccountUsageIdentity {
                    credentials_stamp,
                    ..Default::default()
                },
                "claude",
            );
        }
    };
    match load_credentials() {
        Ok(credentials) => crate::agents::credits::map_account_usage_probe(
            fetch_usage_with_url(&url, &credentials, cli_version),
            crate::agents::AccountUsageIdentity {
                account_key: Some(credentials.account_key.clone()),
                credentials_stamp,
                ..Default::default()
            },
            "claude",
        ),
        Err(err) => crate::agents::credits::map_account_usage_probe(
            Err(err),
            crate::agents::AccountUsageIdentity {
                credentials_stamp,
                ..Default::default()
            },
            "claude",
        ),
    }
}

pub(crate) fn fetch_usage_with_token(
    access_token: &str,
    cli_version: Option<&str>,
) -> Result<AccountUsageSnapshot> {
    fetch_usage_with_url(
        &usage_url()?,
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

pub(crate) fn parse_credentials(bytes: &[u8]) -> Result<ClaudeOauthCredentials> {
    let parsed: CredentialsFile = serde_json::from_slice(bytes)?;
    let Some(oauth) = parsed.claude_ai_oauth else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    let Some(access_token) = oauth.access_token.as_deref().and_then(non_empty_trimmed) else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    let refresh_token = oauth.refresh_token.as_deref().and_then(non_empty_trimmed);
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
    parse_usage_response(&body)
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

fn usage_url() -> Result<String> {
    resolve_usage_url(std::env::var(URL_ENV).ok().as_deref())
}

fn resolve_usage_url(override_url: Option<&str>) -> Result<String> {
    let Some(candidate) = override_url.filter(|value| !value.trim().is_empty()) else {
        return Ok(DEFAULT_USAGE_URL.to_owned());
    };
    if trusted_usage_url(candidate, OFFICIAL_HOST) {
        Ok(candidate.to_owned())
    } else {
        Err(ClaudeOauthUsageErr::UntrustedUsageUrl {
            host: url_host(candidate).to_owned(),
        })
    }
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

impl UsageWire {
    fn into_account_usage(self) -> AccountUsageSnapshot {
        AccountUsageSnapshot {
            rate_limits: collect_rate_limits(self.five_hour, self.seven_day),
            extra_credits: collect_extra_usage(self.extra_usage),
            ..Default::default()
        }
    }
}

fn collect_rate_limits(
    five_hour: Option<WindowWire>,
    seven_day: Option<WindowWire>,
) -> Option<AgentRateLimits> {
    let windows: Vec<RateLimitWindow> = [
        window(five_hour, super::account::FIVE_HOUR_MINS),
        window(seven_day, super::account::SEVEN_DAY_MINS),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!windows.is_empty()).then_some(AgentRateLimits { windows })
}

fn window(field: Option<WindowWire>, duration_mins: u32) -> Option<crate::agents::RateLimitWindow> {
    let field = field?;
    let resets_at = field
        .resets_at
        .as_deref()
        .and_then(|raw| raw.parse::<Timestamp>().ok());
    super::account::budget_window(
        field.utilization,
        resets_at,
        duration_mins,
        WindowSource::Authoritative,
    )
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
