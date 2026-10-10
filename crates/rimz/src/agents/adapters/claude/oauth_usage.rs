//! Direct Claude OAuth account usage and manual reset-credit claims.
//!
//! Reads `.credentials.json` under the login's Claude config home and
//! normalizes the provider usage endpoint's response into RimZ's
//! account-window and paid-usage types. Probes are read-only; manual
//! redemption prepares from usage and profile reads, then sends one claim
//! without retries. It never refreshes or writes credentials; retry/backoff
//! of reads and cache writes live in the CLI helper that calls this module.

use std::collections::BTreeMap;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::time::Duration;

use jiff::Timestamp;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::utils::time::{format_local_timestamp, unix_now_ms};

use crate::agents::account::file_mtime_ms;
use crate::agents::account::{
    ProviderCapacity, RedeemHold, RedemptionCode, ResetCreditAction, ResetCreditOffer,
    ResetCreditResult,
};
use crate::agents::capabilities::LaunchCapability;
use crate::agents::context::{AgentRateLimits, RateLimitWindow, WindowSource};
use crate::agents::credits::{
    AccountUsageFault, OAuthHttpErr, oauth_http_get, oauth_http_post_json_once, trusted_usage_url,
    url_host,
};
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
    #[error("claude OAuth usage oauth_not_allowed_for_organization (host {host})")]
    NotEntitled { host: String },
    #[error("claude OAuth profile HTTP {kind} (host {host})")]
    ProfileHttp { kind: HttpErrKind, host: String },
    #[error("claude OAuth profile organization.uuid is empty")]
    EmptyOrganization,
    #[error(
        "claude limit-reset claim outcome is unknown (HTTP {kind}, host {host}); check Claude's Settings > Usage before retrying"
    )]
    ClaimHttp { kind: HttpErrKind, host: String },
    #[error(
        "claude limit-reset claim outcome is unknown (invalid response: {0}); check Claude's Settings > Usage before retrying"
    )]
    ClaimResponse(serde_json::Error),
}

impl crate::agents::credits::AccountUsageReportable for ClaudeOauthUsageErr {
    /// Which fault this failure is: only a transient one reports off-box. Absent credentials, an
    /// expired token, a missing usage scope, and a locally refused URL are
    /// settled states, not faults; plain provider 401/403 is the same settled auth
    /// verdict. Parse and other HTTP failures are.
    fn fault(&self) -> AccountUsageFault {
        if matches!(self, Self::NotEntitled { .. }) {
            return AccountUsageFault::NotEntitled;
        }
        if matches!(
            self,
            Self::NoCredentials
                | Self::TokenExpired
                | Self::MissingScope
                | Self::UntrustedUsageUrl { .. }
        ) || matches!(
            self,
            Self::Http { kind, .. } | Self::ProfileHttp { kind, .. } if kind.is_auth_rejected()
        ) {
            AccountUsageFault::NoCredentials
        } else {
            AccountUsageFault::Transient
        }
    }
}

type Result<T> = std::result::Result<T, ClaudeOauthUsageErr>;

fn redemption_hold(usage: &UsageWire, now: Timestamp) -> Option<RedeemHold> {
    let held = |code, reason| Some(RedeemHold { code, reason });
    let Some(resets) = usage.cedar_ember.as_ref().filter(|resets| resets.eligible) else {
        let reason = usage
            .cedar_ember
            .as_ref()
            .and_then(|resets| resets.ineligible_reason.as_deref());
        return held(
            RedemptionCode::NoCredit,
            reason.map_or_else(
                || "not eligible for limit resets".to_owned(),
                |reason| format!("not eligible for limit resets: {reason}"),
            ),
        );
    };
    if let Some(until) = parse_reset(resets.cooldown_until.as_deref()).filter(|until| *until > now)
    {
        return held(
            RedemptionCode::Cooldown,
            format!("cooldown until {}", format_local_timestamp(until)),
        );
    }
    let Some(grant) = resets.next_grant_id.as_deref().and_then(|id| {
        resets
            .grants
            .iter()
            .find(|grant| grant.id.as_deref() == Some(id))
    }) else {
        return held(RedemptionCode::NoCredit, "no grant selected".to_owned());
    };
    let reason = if grant.paused {
        "selected grant is paused"
    } else if grant.resets_left == 0 {
        "selected grant has no resets left"
    } else if parse_reset(grant.starts_at.as_deref()).is_some_and(|start| start > now) {
        "selected grant has not started"
    } else if parse_reset(grant.ends_at.as_deref()).is_some_and(|end| end < now) {
        "selected grant has expired"
    } else if !grant.usable_now {
        if grant.use_requires_limit && !resets.at_limit {
            "usable only while at the limit"
        } else {
            "selected grant is not usable now"
        }
    } else {
        return None;
    };
    held(RedemptionCode::NoCredit, reason.to_owned())
}

fn endpoint_url(usage_url: &str, segments: &[&str]) -> Result<String> {
    let refused = || ClaudeOauthUsageErr::UntrustedUsageUrl {
        host: url_host(usage_url).to_owned(),
    };
    if !trusted_usage_url(usage_url, OFFICIAL_HOST) {
        return Err(refused());
    }
    let usage = url::Url::parse(usage_url).map_err(|_| refused())?;
    let mut url = url::Url::parse(&usage.origin().ascii_serialization()).map_err(|_| refused())?;
    url.path_segments_mut()
        .map_err(|_| refused())?
        .extend(segments);
    Ok(url.into())
}

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
    ineligible_reason: Option<String>,
    at_limit: bool,
    next_grant_id: Option<String>,
    cooldown_until: Option<String>,
    grants: Vec<ResetGrantWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ResetGrantWire {
    id: Option<String>,
    resets_left: u32,
    starts_at: Option<String>,
    ends_at: Option<String>,
    paused: bool,
    usable_now: bool,
    use_requires_limit: bool,
}

#[derive(Deserialize)]
struct ProfileWire {
    organization: OrganizationWire,
}

#[derive(Deserialize)]
struct OrganizationWire {
    uuid: String,
}

#[derive(Deserialize)]
struct ClaimWire {
    result: ClaimCode,
    #[serde(default)]
    cleared: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClaimCode {
    Reset,
    AlreadyUsed,
    NotLimited,
    Ineligible,
    Unavailable,
    Cooldown,
    #[serde(other)]
    Unknown,
}

struct ClaudeResetCreditAction {
    usage_url: String,
    claim_url: String,
    access_token: String,
    grant_id: Option<String>,
    identity: crate::agents::AccountUsageIdentity,
}

pub(super) fn prepare_reset_credit(
    login_env: &BTreeMap<String, String>,
) -> Result<ResetCreditOffer> {
    let usage_url = usage_url()?;
    let credentials_stamp = credentials_stamp(login_env);
    let credentials = load_credentials(login_env)?;
    // usage_url already parsed and validated the resolved URL.
    let mut preview_url = url::Url::parse(&usage_url).expect("resolved usage URL was parsed");
    preview_url.query_pairs_mut().append_pair("skip_spend", "1");
    let usage: UsageWire =
        serde_json::from_str(&http_get(preview_url.as_str(), &credentials.access_token)?)?;
    let hold = redemption_hold(&usage, Timestamp::now());
    let grant_id = if hold.is_none() {
        usage
            .cedar_ember
            .as_ref()
            .and_then(|resets| resets.next_grant_id.clone())
    } else {
        None
    };
    let profile_url = endpoint_url(&usage_url, &["api", "oauth", "profile"])?;
    let profile: ProfileWire = serde_json::from_str(
        &oauth_http_get(
            &profile_url,
            &oauth_headers(&credentials.access_token),
            "claude: fetching OAuth account profile",
        )
        .map_err(|error| ClaudeOauthUsageErr::ProfileHttp {
            kind: error.kind,
            host: error.host,
        })?,
    )?;
    if profile.organization.uuid.trim().is_empty() {
        return Err(ClaudeOauthUsageErr::EmptyOrganization);
    }
    let claim_url = endpoint_url(
        &usage_url,
        &[
            "api",
            "organizations",
            &profile.organization.uuid,
            "reset_rate_limits",
        ],
    )?;
    let identity = crate::agents::AccountUsageIdentity {
        account_key: Some(credentials.account_key),
        credentials_stamp,
        ..Default::default()
    };
    let usage = usage.into_account_usage();
    let capacity = usage
        .rate_limits
        .map(|limits| ProviderCapacity::from_windows(limits.windows));
    // Every parsed Claude usage body publishes a reset-credit balance, including zero.
    let credits = usage
        .reset_credits
        .expect("Claude usage always sets reset_credits");
    let mut offer = ResetCreditOffer::new(
        capacity,
        credits,
        ClaudeResetCreditAction {
            usage_url,
            claim_url,
            access_token: credentials.access_token,
            grant_id,
            identity,
        },
    );
    offer.hold = hold;
    Ok(offer)
}

impl ResetCreditAction for ClaudeResetCreditAction {
    fn consume(
        self: Box<Self>,
        request_id: &str,
    ) -> std::result::Result<ResetCreditResult, String> {
        let grant_id = self
            .grant_id
            .as_deref()
            .ok_or_else(|| "no grant selected".to_owned())?;
        let body = serde_json::json!({"program": "cedar_ember", "grant_id": grant_id, "request_id": request_id});
        let response = oauth_http_post_json_once(
            &self.claim_url,
            &oauth_headers(&self.access_token),
            &body,
            "claude: claiming a limit reset",
        )
        .map_err(|error| {
            ClaudeOauthUsageErr::ClaimHttp {
                kind: error.kind,
                host: error.host,
            }
            .to_string()
        })?;
        let claim: ClaimWire = serde_json::from_str(&response)
            .map_err(|error| ClaudeOauthUsageErr::ClaimResponse(error).to_string())?;
        let outcome = match claim.result {
            ClaimCode::Reset => RedemptionCode::Reset,
            ClaimCode::AlreadyUsed => RedemptionCode::AlreadyRedeemed,
            ClaimCode::NotLimited => RedemptionCode::NothingToReset,
            ClaimCode::Ineligible | ClaimCode::Unavailable => RedemptionCode::NoCredit,
            ClaimCode::Cooldown => RedemptionCode::Cooldown,
            ClaimCode::Unknown => RedemptionCode::Unknown,
        };
        let (refreshed, refresh_error) = if outcome == RedemptionCode::Reset {
            match fetch_usage_with_url(&self.usage_url, &self.access_token) {
                Ok(usage) => (Some((self.identity, usage)), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (None, None)
        };
        Ok(ResetCreditResult {
            outcome,
            windows_reset: claim.cleared.len() as i64,
            refreshed,
            refresh_error,
        })
    }
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
    oauth_http_get(
        url,
        &oauth_headers(token),
        "claude: fetching OAuth account usage",
    )
    .map_err(classify_usage_http_error)
}

fn classify_usage_http_error(error: OAuthHttpErr) -> ClaudeOauthUsageErr {
    let not_entitled = error.kind == HttpErrKind::Status(403)
        && serde_json::from_str::<serde_json::Value>(&error.body).is_ok_and(|value| {
            value
                .get("error")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|object| {
                    object
                        .values()
                        .any(|value| value.as_str() == Some("oauth_not_allowed_for_organization"))
                })
        });
    if not_entitled {
        return ClaudeOauthUsageErr::NotEntitled { host: error.host };
    }
    ClaudeOauthUsageErr::Http {
        kind: error.kind,
        host: error.host,
    }
}

fn oauth_headers(token: &str) -> [(&'static str, String); 4] {
    [
        ("Authorization", format!("Bearer {token}")),
        ("Accept", "application/json".to_owned()),
        ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ("User-Agent", claude_code_user_agent()),
    ]
}

fn claude_code_user_agent() -> String {
    claude_code_user_agent_from(crate::agents::version::probe_cli_version("claude").as_deref())
}

fn claude_code_user_agent_from(probed: Option<&str>) -> String {
    user_agent(probed.unwrap_or(USER_AGENT_FALLBACK_VERSION))
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
            reset_credits: Some(crate::agents::ResetCredits::normalized(
                count,
                expiries,
                crate::agents::RedeemEffect::KeepsSchedule,
            )),
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
