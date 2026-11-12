//! Provider paid-usage enrichment.
//!
//! Rate-limit windows answer "how much included subscription budget is left".
//! Extra credits answer "how much paid usage beyond those windows is available
//! or has been spent". The source may be a provider account surface, a local
//! API-key spend projection, or a future admin API, so the type keeps each
//! figure optional and lets the renderer state only what is known.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::AccountUsageIdentity;
use crate::agents::account::AccountUsageKey;
use crate::agents::context::{AgentRateLimits, ProviderAccountScope};
use crate::ids::LoginKey;

/// How long an extra-credits/account-usage reading stays displayable. Paid
/// usage is a coarse monthly signal; after a day without a refresh, the
/// dashboard drops the provider-supplied figures rather than showing stale
/// balance as current.
pub(crate) const CREDITS_DISPLAY_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Shared provider extra-credits cache, keyed by login and account scope.
/// Unversioned: grows only by additive, defaulted fields because older builds rewrite it whole.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct CreditsCache {
    pub refreshed_at_ms: u64,
    pub logins: BTreeMap<LoginKey, ProviderCreditsEntry>,
    #[serde(
        default,
        deserialize_with = "AccountUsageKey::deserialize_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub scoped: BTreeMap<AccountUsageKey, ProviderCreditsEntry>,
}

impl CreditsCache {
    /// A kind-wide key reads `logins` whatever the entry's scope, so a caller
    /// can find a sub-provider entry an older build wrote there; a sub-provider
    /// key prefers `scoped`, then a `logins` entry recorded under that scope.
    pub(crate) fn get(&self, key: &AccountUsageKey) -> Option<&ProviderCreditsEntry> {
        if key.scope.is_kind_wide() {
            return self.logins.get(&key.login);
        }
        self.scoped.get(key).or_else(|| {
            self.logins
                .get(&key.login)
                .filter(|entry| entry.scope == key.scope)
        })
    }

    /// The entry a write goes to. Sub-provider scopes live only in `scoped`; a
    /// sub-provider entry an older build left in `logins` moves there first when
    /// the write is kind-wide or to that entry's own scope.
    pub(crate) fn entry_mut(&mut self, key: &AccountUsageKey) -> &mut ProviderCreditsEntry {
        if self.logins.get(&key.login).is_some_and(|entry| {
            !entry.scope.is_kind_wide() && (key.scope.is_kind_wide() || entry.scope == key.scope)
        }) && let Some(legacy) = self.logins.remove(&key.login)
        {
            self.scoped
                .entry(AccountUsageKey::new(
                    key.login.clone(),
                    legacy.scope.clone(),
                ))
                .or_insert(legacy);
        }
        if key.scope.is_kind_wide() {
            return self.logins.entry(key.login.clone()).or_default();
        }
        self.scoped
            .entry(key.clone())
            .or_insert_with(|| ProviderCreditsEntry {
                scope: key.scope.clone(),
                ..Default::default()
            })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProviderCreditsEntry {
    #[serde(default)]
    pub scope: ProviderAccountScope,
    #[serde(default)]
    pub entitlement: Entitlement,
    pub observed_at_ms: u64,
    /// Last OAuth account-usage attempt. Same-identity app-server/realtime
    /// writes preserve this stamp, its published hint and its settled-auth
    /// state; they never advance the OAuth cadence.
    #[serde(default)]
    pub oauth_read_at_ms: u64,
    /// Published account hint under which the last direct read was claimed.
    /// When absent (no published hint, an old cache, or an older build's write),
    /// freshness compares the current hint with the reading's own identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_read_hint: Option<AccountUsageIdentity>,
    /// Last OAuth attempt settled as an auth or entitlement rejection: retried on the long TTL
    /// until the published account hint changes.
    #[serde(default)]
    pub auth_settled: bool,
    /// Credential-source stamp of the reading cached here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_stamp: Option<u64>,
    /// Stable local OAuth account identifier for the facts cached here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    /// Raw provider plan tier from the OAuth usage response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ResetCredits>,
    /// One cross-workspace direct-query lease. Old cache files omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_query_claim: Option<DirectQueryClaim>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DirectQueryClaim {
    pub nonce: Uuid,
    pub claimed_at_ms: u64,
    /// Raw published account hint, before same-scope identity fallbacks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<AccountUsageIdentity>,
    pub requested_scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_stamp: Option<u64>,
    /// Cache-derived owner fallback for a probe that could not resolve its
    /// authoritative account identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight_account_key: Option<String>,
}

pub(crate) fn read_credits_cache(path: &Path) -> CreditsCache {
    crate::disk::atomic::read_json_cache(path)
}

pub(crate) fn entry_is_displayable(entry: &ProviderCreditsEntry, now_ms: u64) -> bool {
    entry.ok
        && now_ms.saturating_sub(entry.observed_at_ms) <= CREDITS_DISPLAY_MAX_AGE.as_millis() as u64
}

/// `true` when direct provider account-usage fetches are disabled for this
/// process (tests, CI, air-gapped runs).
pub(crate) fn oauth_usage_offline() -> bool {
    crate::utils::env::flag_enabled("RIMZ_OAUTH_USAGE_OFFLINE")
}

/// Why an OAuth usage HTTP probe failed, carried structured (so a status code is
/// a Sentry facet) without the request URL's path or query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HttpErrKind {
    /// A response with this non-200 status code.
    Status(u16),
    /// The request never completed (DNS, connect, TLS, or timeout).
    Transport,
    /// The response arrived but its body could not be read.
    Body,
}

impl std::fmt::Display for HttpErrKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(code) => write!(f, "status {code}"),
            Self::Transport => f.write_str("transport"),
            Self::Body => f.write_str("body"),
        }
    }
}

impl HttpErrKind {
    pub(super) fn is_auth_rejected(&self) -> bool {
        matches!(self, Self::Status(401 | 403))
    }

    fn is_transient(&self) -> bool {
        matches!(self, Self::Transport | Self::Body | Self::Status(500..=599))
    }
}

pub(super) struct OAuthHttpErr {
    pub kind: HttpErrKind,
    pub host: String,
    pub body: String,
}

impl std::fmt::Debug for OAuthHttpErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthHttpErr")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// The host authority of `url` — scheme stripped, path/query/fragment and any
/// `userinfo@` removed — the only part of a request URL safe to attach to an
/// off-box error. Std-only; never pulls in a URL parser.
pub(super) fn url_host(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

/// True when `url` targets the provider's official endpoint (exact `https`
/// origin on the default port) or a loopback host. Integration tests drive the
/// probes against local stubs, and a loopback listener already implies local
/// execution; every other origin is refused before a secret-bearing request is
/// built.
pub(super) fn trusted_usage_url(url: &str, official_host: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(domain)) if domain.eq_ignore_ascii_case("localhost") => true,
        Some(url::Host::Domain(domain)) => {
            parsed.scheme() == "https"
                && parsed.port().is_none()
                && domain.eq_ignore_ascii_case(official_host)
        }
        None => false,
    }
}

pub(crate) const OAUTH_HTTP_TIMEOUT_SECS: u64 = 5;
pub(super) const OAUTH_HTTP_MAX_BYTES: u64 = 512 * 1024;
const OAUTH_HTTP_ATTEMPTS: u32 = 3;
const OAUTH_HTTP_RETRY_BACKOFF_MS: u64 = 300;
const OAUTH_HTTP_RETRY_BACKOFF: Duration = Duration::from_millis(OAUTH_HTTP_RETRY_BACKOFF_MS);
/// Longest valid wall time for one fully retried OAuth request.
#[cfg(test)]
pub(crate) const OAUTH_HTTP_MAX_DURATION: Duration = Duration::from_millis(
    OAUTH_HTTP_TIMEOUT_SECS * 1_000 * OAUTH_HTTP_ATTEMPTS as u64
        + OAUTH_HTTP_RETRY_BACKOFF_MS
            * OAUTH_HTTP_ATTEMPTS as u64
            * (OAUTH_HTTP_ATTEMPTS as u64 - 1)
            / 2,
);

/// Bounded GET for provider OAuth usage endpoints: 5s timeout per attempt, 512
/// KiB body cap, host-only breadcrumb. Returns the body, or the error kind plus
/// host authority and bounded error body for the adapter's native classifier.
pub(super) fn oauth_http_get(
    url: &str,
    headers: &[(&str, String)],
    breadcrumb: &str,
) -> std::result::Result<String, OAuthHttpErr> {
    oauth_http_request(url, headers, None, breadcrumb)
}

/// Bounded JSON POST for fixed provider account-usage endpoints. Secret-bearing
/// requests refuse redirects and expose only the destination host and status
/// class to diagnostics; only the adapter may inspect an error body.
pub(super) fn oauth_http_post_json<T: Serialize>(
    url: &str,
    headers: &[(&str, String)],
    body: &T,
    breadcrumb: &str,
) -> std::result::Result<String, OAuthHttpErr> {
    let body = serde_json::to_vec(body).map_err(|_| http_error(url, HttpErrKind::Body))?;
    oauth_http_request(url, headers, Some(&body), breadcrumb)
}

/// Bounded GET without retries, for best-effort profile reads on the account cadence.
pub(super) fn oauth_http_get_once(
    url: &str,
    headers: &[(&str, String)],
    breadcrumb: &str,
) -> std::result::Result<String, OAuthHttpErr> {
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        host = %url_host(url),
        "{}",
        breadcrumb,
    );
    oauth_http_request_once(url, headers, None)
}

/// Bounded, non-retrying JSON POST for claims whose idempotency is unverified.
pub(super) fn oauth_http_post_json_once<T: Serialize>(
    url: &str,
    headers: &[(&str, String)],
    body: &T,
    breadcrumb: &str,
) -> std::result::Result<String, OAuthHttpErr> {
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        host = %url_host(url),
        "{}",
        breadcrumb,
    );
    let body = serde_json::to_vec(body).map_err(|_| http_error(url, HttpErrKind::Body))?;
    oauth_http_request_once(url, headers, Some(&body))
}

fn oauth_http_request(
    url: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    breadcrumb: &str,
) -> std::result::Result<String, OAuthHttpErr> {
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        host = %url_host(url),
        "{}",
        breadcrumb,
    );

    let mut result = oauth_http_request_once(url, headers, body);
    for attempt in 1..OAUTH_HTTP_ATTEMPTS {
        match &result {
            Err(error) if error.kind.is_transient() => {
                tracing::debug!(host = %error.host, attempt, kind = %error.kind, "OAuth usage HTTP retry");
                std::thread::sleep(OAUTH_HTTP_RETRY_BACKOFF * attempt);
                result = oauth_http_request_once(url, headers, body);
            }
            _ => break,
        }
    }
    result
}

fn oauth_http_request_once(
    url: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
) -> std::result::Result<String, OAuthHttpErr> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(OAUTH_HTTP_TIMEOUT_SECS)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut request = ureq::http::Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    if body.is_some() {
        request = request.header("Content-Type", "application/json");
    }
    let response = match body {
        Some(body) => request
            .body(body)
            .map_err(|_| http_error(url, HttpErrKind::Transport))
            .and_then(|request| {
                agent
                    .run(request)
                    .map_err(|_| http_error(url, HttpErrKind::Transport))
            }),
        None => request
            .body(())
            .map_err(|_| http_error(url, HttpErrKind::Transport))
            .and_then(|request| {
                agent
                    .run(request)
                    .map_err(|_| http_error(url, HttpErrKind::Transport))
            }),
    };
    let mut response = response?;
    let status = response.status().as_u16();
    let response_body = response
        .body_mut()
        .with_config()
        .limit(OAUTH_HTTP_MAX_BYTES)
        .read_to_string()
        .map_err(|_| http_error(url, HttpErrKind::Body));
    if status != 200 {
        return Err(OAuthHttpErr {
            body: response_body.unwrap_or_default(),
            ..http_error(url, HttpErrKind::Status(status))
        });
    }
    response_body
}

fn http_error(url: &str, kind: HttpErrKind) -> OAuthHttpErr {
    OAuthHttpErr {
        kind,
        host: url_host(url).to_owned(),
        body: String::new(),
    }
}

/// A provider account-usage reading normalized from a local out-of-band source:
/// included subscription windows plus optional paid extra/API and reset-credit
/// balances.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountUsageSnapshot {
    pub rate_limits: Option<AgentRateLimits>,
    pub extra_credits: Option<ExtraCredits>,
    pub reset_credits: Option<ResetCredits>,
    pub plan: Option<String>,
}

/// Paid usage beyond the subscription windows: Claude extra usage, Codex
/// credits, or API-key spend against a display ceiling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "ExtraCreditsWire", into = "ExtraCreditsWire")]
pub enum ExtraCredits {
    /// The provider says this account cannot use extra paid usage.
    Disabled,
    /// The provider reports no cap on paid extra usage.
    Unlimited,
    /// The source supplied some combination of usage, remaining balance, and
    /// limit. Missing values stay missing; callers may fill a display-only
    /// ceiling when a real limit is absent.
    Known {
        used_usd: Option<f64>,
        remaining_usd: Option<f64>,
        limit_usd: Option<f64>,
    },
}

// Older binaries sharing the credits cache must still accept reported unlimited credit.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExtraCreditsWire {
    Disabled,
    Known {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        used_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unlimited: Option<bool>,
    },
}

impl From<ExtraCreditsWire> for ExtraCredits {
    fn from(credits: ExtraCreditsWire) -> Self {
        match credits {
            ExtraCreditsWire::Disabled => Self::Disabled,
            ExtraCreditsWire::Known {
                unlimited: Some(true),
                ..
            } => Self::Unlimited,
            ExtraCreditsWire::Known {
                used_usd,
                remaining_usd,
                limit_usd,
                ..
            } => Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            },
        }
    }
}

impl From<ExtraCredits> for ExtraCreditsWire {
    fn from(credits: ExtraCredits) -> Self {
        match credits {
            ExtraCredits::Disabled => Self::Disabled,
            ExtraCredits::Unlimited => Self::Known {
                used_usd: None,
                remaining_usd: None,
                limit_usd: None,
                unlimited: Some(true),
            },
            ExtraCredits::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
                unlimited: None,
            },
        }
    }
}

/// Banked rate-limit reset credits from Codex or Claude.
/// A credit that expires unredeemed is wasted, so the dashboard shows the
/// available count and colors the marker by the soonest expiry.
///
/// `count`, `expiries` (one timestamp per dated credit, sorted) and
/// `soonest_expiry` are the flat summary every reader schedules from. Claude
/// also keeps `grants`, one entry per provider grant with its label; Codex
/// reports no grants. [`ResetCredits::accounted`] reads either shape as one
/// list that accounts for every credit of `count`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResetCredits {
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soonest_expiry: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expiries: Vec<Timestamp>,
    /// A credit cached before this field existed is a Codex credit, the only
    /// kind anything redeems, so a missing key reads as `RestartsWindow`.
    #[serde(default = "RedeemEffect::restarts_window")]
    pub effect: RedeemEffect,
    /// Per-grant detail, ordered by expiry with undated grants last. Empty for
    /// Codex and for a record an older build rewrote, which drops the key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<ResetGrant>,
}

/// One provider grant of reset credits, kept as the provider reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetGrant {
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<Timestamp>,
    /// The provider's own label, verbatim; renderers sanitize it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The provider's window tokens this grant clears, untranslated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clears: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<Timestamp>,
}

/// What redeeming a reset credit does to the window's natural reset, as
/// observed per provider: both refill the window, but only some restart it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedeemEffect {
    /// The window restarts a full period from the redemption (Codex).
    RestartsWindow,
    /// The window's reset stays where it was (Claude).
    KeepsSchedule,
}

impl RedeemEffect {
    fn restarts_window() -> Self {
        Self::RestartsWindow
    }
}

impl ResetCredits {
    pub(super) fn normalized(
        count: u32,
        mut expiries: Vec<Timestamp>,
        effect: RedeemEffect,
    ) -> Self {
        expiries.sort_unstable();
        Self {
            count,
            soonest_expiry: expiries.first().copied(),
            expiries,
            effect,
            grants: Vec::new(),
        }
    }

    /// Builds the flat summary from per-grant detail. A grant with nothing
    /// left is dropped, as it adds nothing to the count.
    pub(super) fn from_grants(mut grants: Vec<ResetGrant>, effect: RedeemEffect) -> Self {
        grants.retain(|grant| grant.count > 0);
        sort_by_deadline(&mut grants);
        let count = grants
            .iter()
            .fold(0_u32, |sum, grant| sum.saturating_add(grant.count));
        let expiries = grants
            .iter()
            .filter_map(|grant| Some(std::iter::repeat_n(grant.expiry?, grant.count as usize)))
            .flatten()
            .collect();
        Self {
            grants,
            ..Self::normalized(count, expiries, effect)
        }
    }

    /// Every credit of `count` as grants, soonest deadline first and undated
    /// last: the stored grants, or one unlabelled grant per flat expiry, then
    /// one undated grant for whatever `count` holds beyond them.
    pub fn accounted(&self) -> Vec<ResetGrant> {
        let mut grants = if self.grants.is_empty() {
            let mut expiries = if self.expiries.is_empty() {
                self.soonest_expiry.into_iter().collect()
            } else {
                self.expiries.clone()
            };
            expiries.sort_unstable();
            expiries.truncate(self.count as usize);
            expiries
                .into_iter()
                .map(|expiry| ResetGrant {
                    count: 1,
                    expiry: Some(expiry),
                    ..Default::default()
                })
                .collect()
        } else {
            self.grants.clone()
        };
        let listed = grants
            .iter()
            .fold(0_u32, |sum, grant| sum.saturating_add(grant.count));
        if let Some(rest) = self.count.checked_sub(listed).filter(|rest| *rest > 0) {
            grants.push(ResetGrant {
                count: rest,
                ..Default::default()
            });
        }
        sort_by_deadline(&mut grants);
        grants
    }
}

/// Soonest expiry first, undated last, the provider's order kept among equals.
fn sort_by_deadline(grants: &mut [ResetGrant]) {
    grants.sort_by_key(|grant| (grant.expiry.is_none(), grant.expiry));
}

impl ExtraCredits {
    pub fn known(
        used_usd: Option<f64>,
        remaining_usd: Option<f64>,
        limit_usd: Option<f64>,
    ) -> Self {
        Self::Known {
            used_usd: clean_usd(used_usd),
            remaining_usd: clean_usd(remaining_usd),
            limit_usd: clean_usd(limit_usd),
        }
    }

    pub(crate) fn with_limit_if_missing(self, fallback_limit_usd: Option<f64>) -> Self {
        match self {
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => Self::Known {
                used_usd,
                remaining_usd,
                limit_usd: limit_usd.or_else(|| clean_usd(fallback_limit_usd)),
            },
            Self::Unlimited => Self::Unlimited,
            Self::Disabled => Self::Disabled,
        }
    }

    pub(crate) fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Whether this extra paid budget is known to be exhausted.
    fn is_exhausted(&self) -> bool {
        match self {
            Self::Unlimited => false,
            Self::Disabled => true,
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => {
                remaining_usd.is_some_and(|remaining| remaining <= 0.0)
                    || used_usd
                        .zip(*limit_usd)
                        .is_some_and(|(used, limit)| limit > 0.0 && used >= limit)
            }
        }
    }

    /// Whether extra paid usage may be available. Unknown values count as usable
    /// because no source has proven the budget is exhausted.
    pub(crate) fn is_usable(&self) -> bool {
        !self.is_disabled() && !self.is_exhausted()
    }

    /// Remaining percentage for a draining bar, when enough data is known.
    pub(crate) fn remaining_percentage(&self) -> Option<u8> {
        match self {
            Self::Unlimited => None,
            Self::Disabled => Some(0),
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => {
                let limit = limit_usd.filter(|limit| *limit > 0.0)?;
                let remaining = if let Some(remaining) = remaining_usd {
                    *remaining
                } else {
                    limit - (*used_usd)?
                };
                Some(((remaining.max(0.0).min(limit) / limit) * 100.0).round() as u8)
            }
        }
    }

    /// Remaining dollars, direct or derived from `limit - used`, when known.
    pub(crate) fn remaining_usd_left(&self) -> Option<f64> {
        match self {
            Self::Unlimited => None,
            Self::Disabled => Some(0.0),
            Self::Known {
                used_usd,
                remaining_usd,
                limit_usd,
            } => remaining_usd.or_else(|| {
                limit_usd
                    .zip(*used_usd)
                    .map(|(limit, used)| (limit - used).max(0.0))
            }),
        }
    }

    pub(crate) fn limit_usd(&self) -> Option<f64> {
        match self {
            Self::Known { limit_usd, .. } => *limit_usd,
            Self::Unlimited => None,
            Self::Disabled => Some(0.0),
        }
    }
}

fn clean_usd(value: Option<f64>) -> Option<f64> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.max(0.0))
}

/// The outcome of an adapter's direct account-usage query, mirroring the
/// typed discipline of [`AccountProbe`](super::account::AccountProbe) so the
/// shared refresh driver can key its cache TTL on the arm, not just the value:
///
/// - `Found` — a usage reading to merge.
/// - `NoCredentials` — the probe ran and confidently found nothing to fetch (no
///   OAuth login, an API-key-only file, an expired token or provider-rejected
///   token, a provider with no quota surface). A settled state, logged at debug.
/// - `Failed` — the probe could not complete (unreadable file, parse error, HTTP
///   error). Transient, logged at warn, retried on the short TTL.
/// - `NotEntitled` — credentials exist but the provider refuses subscription
///   access. Settled, independent of login health.
/// - `Unsupported` — the adapter exposes no direct account-usage probe (the trait
///   default). Nothing to spawn.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountUsageProbe {
    Found {
        identity: AccountUsageIdentity,
        snapshot: AccountUsageSnapshot,
    },
    NoCredentials(AccountUsageIdentity),
    NotEntitled(AccountUsageIdentity),
    Failed(AccountUsageIdentity),
    Unsupported,
}

/// The fault behind an account-usage error. Implemented by each
/// adapter's account-usage error so [`map_account_usage_probe`] can fold every
/// adapter's result through one classifier instead of a hand-rolled match per
/// adapter. The "report" set (HTTP/IO/parse faults) maps to `Failed`; the silent
/// set (absent/api-key/expired credentials and locally refused configuration)
/// maps to `NoCredentials`; subscription-access rejection maps to `NotEntitled`.
pub(super) trait AccountUsageReportable {
    fn fault(&self) -> AccountUsageFault;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccountUsageFault {
    Transient,
    NoCredentials,
    NotEntitled,
}

/// A login's subscription access, independent of whether its credentials exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entitlement {
    #[default]
    Ok,
    Lapsed {
        since_ms: u64,
    },
}

/// Fold an adapter's `Result<AccountUsageSnapshot, E>` into the shared
/// [`AccountUsageProbe`], logging once at the right level. The single home for the
/// debug-vs-warn split, so every adapter's `probe_account_usage` is a one-line
/// delegation to its account-usage fetcher. Every provider reports the static
/// `oauth_usage` operation so Sentry groups them together; the provider tag and
/// error's host authority keep the source visible.
pub(super) fn map_account_usage_probe<E>(
    result: std::result::Result<AccountUsageSnapshot, E>,
    identity: AccountUsageIdentity,
    provider: &'static str,
) -> AccountUsageProbe
where
    E: AccountUsageReportable + std::error::Error + 'static,
{
    match result {
        Ok(snapshot) => AccountUsageProbe::Found { identity, snapshot },
        Err(err) => match err.fault() {
            AccountUsageFault::Transient => {
                tracing::warn!(
                    tags.operation = "oauth_usage",
                    tags.provider = provider,
                    error = &err as &dyn std::error::Error,
                    "OAuth account usage fetch failed",
                );
                AccountUsageProbe::Failed(identity)
            }
            AccountUsageFault::NoCredentials => {
                tracing::debug!(error = %err, provider, "OAuth account usage unavailable");
                AccountUsageProbe::NoCredentials(identity)
            }
            AccountUsageFault::NotEntitled => {
                tracing::debug!(error = %err, provider, "OAuth account usage unavailable");
                AccountUsageProbe::NotEntitled(identity)
            }
        },
    }
}

#[cfg(test)]
mod tests;
