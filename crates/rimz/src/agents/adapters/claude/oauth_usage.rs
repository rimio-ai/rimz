//! Direct Claude OAuth account-usage probe.
//!
//! This is a read-only fallback over Claude Code's local OAuth credentials. It
//! reads `.credentials.json` under the login's Claude config home, calls the
//! provider usage endpoint, and normalizes the response into RimZ's
//! account-window and paid-usage types. It never refreshes or writes
//! credentials; retry/backoff and cache writes live in the CLI helper that
//! calls this module.

use std::collections::BTreeMap;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::utils::time::unix_now_ms;

use crate::agents::account::file_mtime_ms;
use crate::agents::capabilities::LaunchCapability;
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{oauth_http_get, trusted_usage_url, url_host};
use crate::agents::payload::non_empty_trimmed;
use crate::agents::{AccountUsageSnapshot, ExtraCredits, HttpErrKind};

const DEFAULT_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OFFICIAL_HOST: &str = "api.anthropic.com";
const URL_ENV: &str = "RIMZ_CLAUDE_OAUTH_USAGE_URL";
const USER_AGENT_FALLBACK_VERSION: &str = "2.1.283";
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

type Result<T> = std::result::Result<T, ClaudeOauthUsageErr>;

#[derive(Debug, Clone, PartialEq)]
struct ClaudeOauthCredentials {
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
    limits: Vec<LimitWire>,
    cedar_ember: Option<LimitResetsWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LimitResetsWire {
    eligible: bool,
    grants: Vec<ResetGrantWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResetGrantWire {
    resets_left: u32,
    ends_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LimitWire {
    kind: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
    scope: Option<LimitScopeWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LimitScopeWire {
    model: Option<LimitModelWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LimitModelWire {
    display_name: Option<String>,
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

pub(super) fn probe_usage(
    login_env: &BTreeMap<String, String>,
) -> crate::agents::AccountUsageProbe {
    let credentials_stamp = credentials_stamp(login_env);
    let (identity, result) = match usage_url()
        .and_then(|url| load_credentials(login_env).map(|credentials| (url, credentials)))
    {
        Ok((url, credentials)) => (
            crate::agents::AccountUsageIdentity {
                account_key: Some(credentials.account_key.clone()),
                credentials_stamp,
                ..Default::default()
            },
            fetch_usage_with_url(&url, &credentials.access_token),
        ),
        Err(err) => (
            crate::agents::AccountUsageIdentity {
                credentials_stamp,
                ..Default::default()
            },
            Err(err),
        ),
    };
    crate::agents::credits::map_account_usage_probe(result, identity, "claude")
}

pub(in crate::agents) fn fetch_usage_with_token(
    access_token: &str,
) -> Result<AccountUsageSnapshot> {
    fetch_usage_with_url(&usage_url()?, access_token.trim())
}

fn load_credentials(login_env: &BTreeMap<String, String>) -> Result<ClaudeOauthCredentials> {
    parse_credentials(&read_credentials_bytes(login_env)?)
}

pub(super) fn load_account_key(login_env: &BTreeMap<String, String>) -> Result<String> {
    parse_account_key(&read_credentials_bytes(login_env)?)
}

fn read_credentials_bytes(login_env: &BTreeMap<String, String>) -> Result<Vec<u8>> {
    let path = credentials_path(login_env).ok_or(ClaudeOauthUsageErr::NoCredentials)?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if names_config_dir(login_env) {
                return Err(ClaudeOauthUsageErr::NoCredentials);
            }
            read_keychain_credentials_bytes()
        }
        Err(err) => Err(ClaudeOauthUsageErr::Io(err)),
    }
}

/// Whether `CLAUDE_CONFIG_DIR` names a directory, parsed as `config_home`
/// parses it, so a value that resolves to `$HOME/.claude` (such as `","`)
/// still reaches the keychain like an unset one.
fn names_config_dir(login_env: &BTreeMap<String, String>) -> bool {
    super::remote_consent::configured_dir(login_env.get("CLAUDE_CONFIG_DIR").map(String::as_str))
        .is_some()
}

fn read_keychain_credentials_bytes() -> Result<Vec<u8>> {
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
        Ok(output.stdout)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ClaudeOauthUsageErr::NoCredentials)
    }
}

fn credentials_path(login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    super::ClaudeAdapter
        .config_home(login_env)
        .map(|home| home.join(".credentials.json"))
}

pub(super) fn credentials_stamp(login_env: &BTreeMap<String, String>) -> Option<u64> {
    file_mtime_ms(&credentials_path(login_env)?)
}

fn parse_credentials(bytes: &[u8]) -> Result<ClaudeOauthCredentials> {
    let parsed: CredentialsFile = serde_json::from_slice(bytes)?;
    let Some(oauth) = parsed.claude_ai_oauth else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    let Some(access_token) = oauth.access_token.as_deref().and_then(non_empty_trimmed) else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    let scopes = oauth.scopes.as_deref().unwrap_or_default();
    if !scopes.iter().any(|scope| scope == "user:profile") {
        return Err(ClaudeOauthUsageErr::MissingScope);
    }
    let Some(expires_at) = oauth.expires_at else {
        return Err(ClaudeOauthUsageErr::TokenExpired);
    };
    if expires_at <= unix_now_ms() as i64 {
        return Err(ClaudeOauthUsageErr::TokenExpired);
    }
    Ok(ClaudeOauthCredentials {
        access_token,
        account_key: oauth_account_key(&oauth).ok_or(ClaudeOauthUsageErr::NoCredentials)?,
    })
}

fn parse_account_key(bytes: &[u8]) -> Result<String> {
    let parsed: CredentialsFile = serde_json::from_slice(bytes)?;
    let Some(oauth) = parsed.claude_ai_oauth else {
        return Err(ClaudeOauthUsageErr::NoCredentials);
    };
    oauth_account_key(&oauth).ok_or(ClaudeOauthUsageErr::NoCredentials)
}

fn oauth_account_key(oauth: &ClaudeAiOauth) -> Option<String> {
    if let Some(refresh_token) = oauth.refresh_token.as_deref().and_then(non_empty_trimmed) {
        return Some(account_key("refresh-token", &refresh_token));
    }
    let access_token = oauth.access_token.as_deref().and_then(non_empty_trimmed)?;
    Some(account_key("access-token", &access_token))
}

fn fetch_usage_with_url(url: &str, access_token: &str) -> Result<AccountUsageSnapshot> {
    let body = http_get(url, access_token)?;
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
    let candidate = override_url
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_USAGE_URL);
    if !trusted_usage_url(candidate, OFFICIAL_HOST) {
        return Err(ClaudeOauthUsageErr::UntrustedUsageUrl {
            host: url_host(candidate).to_owned(),
        });
    }
    let mut url =
        url::Url::parse(candidate).map_err(|_| ClaudeOauthUsageErr::UntrustedUsageUrl {
            host: url_host(candidate).to_owned(),
        })?;
    url.query_pairs_mut().append_pair("cedar_ember", "1");
    Ok(url.into())
}

fn http_get(url: &str, token: &str) -> Result<String> {
    let headers = [
        ("Authorization", format!("Bearer {token}")),
        ("Accept", "application/json".to_owned()),
        ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ("User-Agent", claude_code_user_agent()),
    ];
    oauth_http_get(url, &headers, "claude: fetching OAuth account usage")
        .map_err(|(kind, host)| ClaudeOauthUsageErr::Http { kind, host })
}

fn claude_code_user_agent() -> String {
    user_agent(
        crate::agents::version::probe_cli_version("claude")
            .as_deref()
            .unwrap_or(USER_AGENT_FALLBACK_VERSION),
    )
}

fn user_agent(version: &str) -> String {
    format!("claude-cli/{version} (external, cli)")
}

fn parse_usage_response(body: &str) -> Result<AccountUsageSnapshot> {
    Ok(serde_json::from_str::<UsageWire>(body)?.into_account_usage())
}

impl UsageWire {
    fn into_account_usage(self) -> AccountUsageSnapshot {
        let mut count: u32 = 0;
        let mut expiries = Vec::new();
        if let Some(resets) = self.cedar_ember.filter(|resets| resets.eligible) {
            for grant in resets.grants {
                count = count.saturating_add(grant.resets_left);
                if let Some(expiry) = parse_reset(grant.ends_at.as_deref()) {
                    expiries.extend(std::iter::repeat_n(expiry, grant.resets_left as usize));
                }
            }
        }
        AccountUsageSnapshot {
            rate_limits: collect_rate_limits(self.five_hour, self.seven_day, self.limits),
            extra_credits: collect_extra_usage(self.extra_usage),
            reset_credits: Some(crate::agents::ResetCredits::normalized(count, expiries)),
            ..Default::default()
        }
    }
}

fn collect_rate_limits(
    five_hour: Option<WindowWire>,
    seven_day: Option<WindowWire>,
    limits: Vec<LimitWire>,
) -> Option<AgentRateLimits> {
    let mut windows: Vec<RateLimitWindow> = [
        window(five_hour, super::account::FIVE_HOUR_MINS),
        window(seven_day, super::account::SEVEN_DAY_MINS),
    ]
    .into_iter()
    .flatten()
    .collect();
    windows.extend(limits.into_iter().filter_map(|limit| {
        if limit.kind.as_deref() != Some("weekly_scoped") {
            return None;
        }
        let display_name = limit.scope?.model?.display_name?;
        let display_name = display_name.trim();
        if display_name.is_empty() {
            return None;
        }
        super::account::model_sub_cap_window(
            display_name,
            limit.percent,
            parse_reset(limit.resets_at.as_deref()),
            super::account::SEVEN_DAY_MINS,
            WindowSource::Authoritative,
        )
    }));
    (!windows.is_empty()).then_some(AgentRateLimits { windows })
}

fn window(field: Option<WindowWire>, duration_mins: u32) -> Option<crate::agents::RateLimitWindow> {
    let field = field?;
    let resets_at = parse_reset(field.resets_at.as_deref());
    super::account::budget_window(
        field.utilization,
        resets_at,
        duration_mins,
        WindowSource::Authoritative,
    )
}

fn parse_reset(raw: Option<&str>) -> Option<Timestamp> {
    raw.and_then(|raw| raw.parse().ok())
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

#[cfg(test)]
#[path = "tests/oauth_usage.rs"]
mod tests;
