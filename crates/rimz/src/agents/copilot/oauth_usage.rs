//! Best-effort GitHub Copilot plan and included-quota probe.

use std::ffi::OsString;

use jiff::Timestamp;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::agents::context::{
    AgentRateLimits, RateLimitWindow, RateLimitWindowScope, WindowSource,
};
use crate::agents::credits::oauth_http_get;
use crate::agents::{AccountUsageSnapshot, HttpErrKind};

const TOKEN_VARS: [&str; 3] = ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
const HOST_VARS: [&str; 2] = ["COPILOT_GH_HOST", "GH_HOST"];

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("copilot GitHub credentials not found")]
    NoCredentials,
    #[error("copilot GitHub host is malformed")]
    InvalidHost,
    #[error("copilot usage endpoint is unavailable for this account or host")]
    Unavailable,
    #[error("copilot usage response has no usable plan or quota")]
    MalformedResponse,
    #[error("copilot usage response has an invalid quota reset date")]
    InvalidReset,
    #[error("parsing copilot usage response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("copilot usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
}

impl crate::agents::credits::AccountUsageReportable for Error {
    fn should_report(&self) -> bool {
        !matches!(self, Self::NoCredentials | Self::Unavailable)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Normalized GitHub web host. Request paths always use HTTPS and a separately
/// derived API authority, so schemes and paths from `GH_HOST` cannot escape the
/// bounded Copilot endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitHubHost(String);

impl GitHubHost {
    pub(super) fn public() -> Self {
        Self("github.com".to_owned())
    }

    pub(super) fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(Error::InvalidHost);
        }
        let authority_and_path = if let Some((scheme, rest)) = raw.split_once("://") {
            if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
                return Err(Error::InvalidHost);
            }
            rest
        } else {
            raw
        };
        let authority = authority_and_path
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        if authority.is_empty()
            || authority.contains('@')
            || authority.chars().any(char::is_whitespace)
        {
            return Err(Error::InvalidHost);
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => {
                let port = port.parse::<u16>().map_err(|_| Error::InvalidHost)?;
                if port == 0 {
                    return Err(Error::InvalidHost);
                }
                (host, Some(port))
            }
            Some(_) => return Err(Error::InvalidHost),
            None => (authority, None),
        };
        let host = host.trim_matches('.').to_ascii_lowercase();
        if host.is_empty()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            })
        {
            return Err(Error::InvalidHost);
        }
        Ok(Self(match port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    fn api_authority(&self) -> String {
        let (host, port) = self
            .0
            .rsplit_once(':')
            .map_or((self.0.as_str(), None), |(host, port)| (host, Some(port)));
        let api_host = if host == "github.com" {
            "api.github.com".to_owned()
        } else if host.starts_with("api.") {
            host.to_owned()
        } else {
            format!("api.{host}")
        };
        port.map_or(api_host.clone(), |port| format!("{api_host}:{port}"))
    }

    fn usage_url(&self) -> String {
        format!("https://{}/copilot_internal/user", self.api_authority())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Credentials {
    token: String,
    host: GitHubHost,
}

pub(super) fn probe() -> crate::agents::AccountUsageProbe {
    let credentials = match credentials_from(|key| std::env::var_os(key)) {
        Ok(credentials) => credentials,
        Err(error) => {
            return crate::agents::credits::map_account_usage_probe(
                Err(error),
                Default::default(),
                "copilot",
            );
        }
    };
    let identity = crate::agents::AccountUsageIdentity {
        account_key: Some(credentials.fingerprint()),
        ..Default::default()
    };
    crate::agents::credits::map_account_usage_probe(fetch_with(&credentials), identity, "copilot")
}

pub(super) fn account_key() -> Option<String> {
    credentials_from(|key| std::env::var_os(key))
        .ok()
        .map(|credentials| credentials.fingerprint())
}

pub(super) fn has_environment_token() -> bool {
    first_nonempty(&TOKEN_VARS, |key| std::env::var_os(key)).is_some()
}

fn credentials_from(mut env: impl FnMut(&str) -> Option<OsString>) -> Result<Credentials> {
    let token = first_nonempty(&TOKEN_VARS, &mut env).ok_or(Error::NoCredentials)?;
    let host = first_nonempty(&HOST_VARS, &mut env)
        .map(|host| GitHubHost::parse(&host))
        .transpose()?
        .unwrap_or_else(GitHubHost::public);
    Ok(Credentials { token, host })
}

fn first_nonempty(keys: &[&str], mut env: impl FnMut(&str) -> Option<OsString>) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = env(key)?;
        let value = value.to_string_lossy();
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

impl Credentials {
    fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(self.host.as_str().as_bytes());
        digest.update([0]);
        digest.update(self.token.as_bytes());
        format!("copilot:{}", hex::encode(digest.finalize()))
    }
}

fn fetch_with(credentials: &Credentials) -> Result<AccountUsageSnapshot> {
    let headers = [
        ("Authorization", format!("token {}", credentials.token)),
        ("Accept", "application/json".to_owned()),
        ("Editor-Version", "vscode/1.96.2".to_owned()),
        ("Editor-Plugin-Version", "copilot-chat/0.26.7".to_owned()),
        ("User-Agent", "GitHubCopilotChat/0.26.7".to_owned()),
        ("X-Github-Api-Version", "2025-04-01".to_owned()),
    ];
    let body = oauth_http_get(
        &credentials.host.usage_url(),
        &headers,
        "copilot: fetching GitHub account usage",
    )
    .map_err(map_http_error)?;
    parse_response(&body)
}

fn map_http_error((kind, host): (HttpErrKind, String)) -> Error {
    match kind {
        HttpErrKind::Status(401 | 403 | 404) => Error::Unavailable,
        _ => Error::Http { kind, host },
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsageWire {
    copilot_plan: Option<String>,
    #[serde(rename = "token_based_billing")]
    _token_based_billing: Option<bool>,
    quota_reset_date: Option<String>,
    quota_snapshots: Option<QuotaSnapshotsWire>,
    monthly_quotas: Option<QuotaCountsWire>,
    limited_user_quotas: Option<QuotaCountsWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QuotaSnapshotsWire {
    premium_interactions: Option<QuotaWire>,
    chat: Option<QuotaWire>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QuotaWire {
    entitlement: Option<FlexibleNumber>,
    remaining: Option<FlexibleNumber>,
    percent_remaining: Option<FlexibleNumber>,
    #[serde(rename = "quota_id")]
    _quota_id: Option<String>,
    unlimited: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct QuotaCountsWire {
    chat: Option<FlexibleNumber>,
    completions: Option<FlexibleNumber>,
}

#[derive(Clone, Copy, Debug)]
struct FlexibleNumber(f64);

impl<'de> Deserialize<'de> for FlexibleNumber {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NumberVisitor;

        impl Visitor<'_> for NumberVisitor {
            type Value = FlexibleNumber;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a finite number or numeric string")
            }

            fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                finite(value)
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                finite(value as f64)
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                finite(value as f64)
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                finite(value.trim().parse::<f64>().map_err(E::custom)?)
            }
        }

        fn finite<E: de::Error>(value: f64) -> std::result::Result<FlexibleNumber, E> {
            value
                .is_finite()
                .then_some(FlexibleNumber(value))
                .ok_or_else(|| E::custom("number is not finite"))
        }

        deserializer.deserialize_any(NumberVisitor)
    }
}

pub(super) fn parse_response(body: &str) -> Result<AccountUsageSnapshot> {
    let wire: UsageWire = serde_json::from_str(body)?;
    let reset = parse_optional_reset(wire.quota_reset_date.as_deref())?;
    let modern = wire.quota_snapshots.unwrap_or_default();
    let premium = normalized_quota(modern.premium_interactions.as_ref()).or_else(|| {
        normalized_legacy_quota(
            wire.monthly_quotas
                .as_ref()
                .and_then(|quota| quota.completions),
            wire.limited_user_quotas
                .as_ref()
                .and_then(|quota| quota.completions),
        )
    });
    let chat = normalized_quota(modern.chat.as_ref()).or_else(|| {
        normalized_legacy_quota(
            wire.monthly_quotas.as_ref().and_then(|quota| quota.chat),
            wire.limited_user_quotas
                .as_ref()
                .and_then(|quota| quota.chat),
        )
    });
    let mut windows = Vec::new();
    if let Some(quota) = premium {
        windows.push(quota.into_window("premium_interactions", "prm", reset));
    }
    if let Some(quota) = chat {
        windows.push(quota.into_window("chat", "cht", reset));
    }
    let plan = wire
        .copilot_plan
        .map(|plan| plan.trim().to_owned())
        .filter(|plan| !plan.is_empty());
    if plan.is_none() && windows.is_empty() {
        return Err(Error::MalformedResponse);
    }
    Ok(AccountUsageSnapshot {
        rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
        plan,
        ..Default::default()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NormalizedQuota {
    used_percentage: u8,
    unlimited: bool,
}

impl NormalizedQuota {
    fn into_window(self, id: &str, label: &str, reset: Option<Timestamp>) -> RateLimitWindow {
        RateLimitWindow {
            scope: Some(RateLimitWindowScope {
                id: id.to_owned(),
                label: label.to_owned(),
            }),
            used_percentage: Some(self.used_percentage),
            resets_at: if self.unlimited { None } else { reset },
            duration_mins: None,
            observed_at: None,
            source: WindowSource::Authoritative,
            lifted: self.unlimited,
        }
    }
}

fn normalized_quota(quota: Option<&QuotaWire>) -> Option<NormalizedQuota> {
    let quota = quota?;
    if quota.unlimited {
        return Some(NormalizedQuota {
            used_percentage: 0,
            unlimited: true,
        });
    }
    if quota.entitlement.map(|value| value.0) == Some(0.0)
        && quota.remaining.map(|value| value.0) == Some(0.0)
    {
        return None;
    }
    let percent_remaining = quota.percent_remaining.map(|value| value.0).or_else(|| {
        let entitlement = quota.entitlement?.0;
        let remaining = quota.remaining?.0;
        (entitlement > 0.0).then_some(remaining / entitlement * 100.0)
    })?;
    Some(NormalizedQuota {
        used_percentage: clamp_percentage(100.0 - percent_remaining),
        unlimited: false,
    })
}

fn normalized_legacy_quota(
    entitlement: Option<FlexibleNumber>,
    remaining: Option<FlexibleNumber>,
) -> Option<NormalizedQuota> {
    let entitlement = entitlement?.0;
    let remaining = remaining?.0;
    if entitlement <= 0.0 {
        return None;
    }
    Some(NormalizedQuota {
        used_percentage: clamp_percentage(100.0 - remaining / entitlement * 100.0),
        unlimited: false,
    })
}

fn clamp_percentage(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

fn parse_optional_reset(value: Option<&str>) -> Result<Option<Timestamp>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if let Ok(timestamp) = value.parse::<Timestamp>() {
        return Ok(Some(timestamp));
    }
    if value.len() == 10 {
        return format!("{value}T00:00:00Z")
            .parse::<Timestamp>()
            .map(Some)
            .map_err(|_| Error::InvalidReset);
    }
    Err(Error::InvalidReset)
}

#[cfg(test)]
#[path = "tests/oauth_usage.rs"]
mod tests;
