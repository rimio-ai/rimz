//! Best-effort provider account probe contract.
//!
//! Account/plan facts are account-scoped, not session-scoped, and some never
//! ride the session context: Claude's saved tier comes from `claude auth status`,
//! with a bound live profile override, not its statusline. Adapters normalize
//! these out-of-band facts through account capabilities; this module owns
//! [`AccountProbe`] and the profile state the producer folds onto the dashboard.
//!
//! Producer-only: a probe may fork a subprocess, so the room host runs
//! it and publishes the result to the shared `accounts.json` cache (TTL'd,
//! single-flighted like the diff stats); consumer tabs read that cache and never
//! fork. A probe is a pure read — the cross-process memoization lives one layer
//! up, in the sidebar producer cache.
//!
//! A probe also detects a *logged-in but idle* provider — one with no active
//! session this run — so the dashboard can show substantive accounts and their
//! budgets between turns. A live session's richer context still wins where both
//! exist.
//!
//! Best-effort by contract: a missing binary, a logged-out account, or
//! unparseable output yields no account. It never fails a snapshot — account is
//! enrichment, never correctness.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use super::{AgentAccount, RoomLoginSet};
use super::{AgentRateLimits, ProviderAccountScope, RateLimitWindow, context::RateLimitWindowKey};
use crate::RuntimePaths;
use crate::ids::LoginKey;

/// Informational account and CLI-version probes are best-effort enrichment.
/// Bound every subprocess so one installed but wedged CLI cannot hold the
/// shared account cache producer indefinitely.
pub(crate) const INFORMATIONAL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests;

/// The producer's published account probe state, keyed by login so one
/// transient failure retries without expiring every account's successful read.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsCache {
    pub logins: BTreeMap<LoginKey, ProviderRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub probed_at_ms: u64,
    #[serde(default, skip_serializing_if = "super::is_zero_u32")]
    pub consecutive_failures: u32,
    /// Start of the version read, aged independently of provisional retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_probed_at_ms: Option<u64>,
    /// A failed probe backs off from `ACCOUNTS_RETRY_TTL` to `ACCOUNTS_TTL`;
    /// success rides `ACCOUNTS_TTL`, or the retry floor while provisional.
    /// A moved adapter stamp makes either outcome due behind the retry floor.
    pub ok: bool,
    /// The adapter's inputs stamp read before the probe that wrote this record.
    /// Absent for an adapter without one and on a record an older build wrote,
    /// both of which wait out the TTL. A later change to the stamp's encoding
    /// costs one probe each time binaries of both encodings alternate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs_stamp: Option<u64>,
    /// Probed account facts. `None` on an `ok` record is a logged-out result,
    /// but a logout can also carry the CLI version, so `login` decides.
    pub account: Option<AgentAccount>,
    /// What the last confident probe found. Absent on an unavailable probe
    /// and on a record an older build wrote, which `ProviderStatus::from_record`
    /// reads from `ok` and `account`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<RecordedLogin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<AccountProbeError>,
    /// Last profile attempt and answer, independent of account availability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AccountProfileState>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountProfileState {
    pub attempted_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<AccountProfileAnswer>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountProfileAnswer {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub read_at_ms: u64,
}

impl std::fmt::Debug for AccountProfileAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountProfileAnswer")
            .field("owner", &"<redacted>")
            .field("basis", &self.basis)
            .field("plan", &self.plan)
            .field("read_at_ms", &self.read_at_ms)
            .finish()
    }
}

/// A confident probe's login outcome as `ProviderRecord` stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordedLogin {
    LoggedIn,
    LoggedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    LoggedIn,
    LoggedOut,
    Unavailable,
}

impl ProviderRecord {
    /// An older build's `ok` record whose account carries nothing but the CLI
    /// version: it wrote those bytes for a logout and for a login the provider
    /// reported no facts about.
    pub fn login_is_ambiguous(&self) -> bool {
        self.ok
            && self.login.is_none()
            && self.account.as_ref().is_some_and(|account| {
                let facts = AgentAccount {
                    version: None,
                    ..account.clone()
                };
                account.version.is_some() && facts == AgentAccount::default()
            })
    }
}

impl ProviderStatus {
    /// The recorded login when the probe stored one. A record without one
    /// reads from `ok` and `account` as an older build wrote them, and its
    /// ambiguous shape is unknown rather than a guess.
    pub fn from_record(record: Option<&ProviderRecord>) -> Self {
        let Some(record) = record.filter(|record| record.ok) else {
            return Self::Unavailable;
        };
        match record.login {
            Some(RecordedLogin::LoggedIn) => Self::LoggedIn,
            Some(RecordedLogin::LoggedOut) => Self::LoggedOut,
            None if record.login_is_ambiguous() => Self::Unavailable,
            None if record.account.is_some() => Self::LoggedIn,
            None => Self::LoggedOut,
        }
    }
}

/// Read the producer's published account cache, or an empty cache on a cold,
/// corrupt, or old-schema file. Read-only and fork-free.
pub(crate) fn read_accounts_cache(path: &Path) -> AccountsCache {
    crate::disk::atomic::read_json_cache(path)
}

/// Exact identity of one provider account whose authoritative usage may drive
/// launch-time controls.
///
/// The account key is an opaque, non-secret fingerprint. Its value stays out
/// of `Debug` and user-facing output; equality is the only control operation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountBinding {
    scope: ProviderAccountScope,
    account_key: String,
}

impl std::fmt::Debug for ProviderAccountBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAccountBinding")
            .field("scope", &self.scope)
            .field("account_key", &"<redacted>")
            .finish()
    }
}

impl ProviderAccountBinding {
    pub(super) fn new(scope: ProviderAccountScope, account_key: String) -> Option<Self> {
        (!account_key.trim().is_empty()).then_some(Self { scope, account_key })
    }

    pub fn scope(&self) -> &ProviderAccountScope {
        &self.scope
    }

    pub(crate) fn account_key(&self) -> &str {
        &self.account_key
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "testkit"))]
    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value)
            .ok()
            .filter(|binding: &Self| !binding.account_key.trim().is_empty())
    }

    fn display_label(&self, kind: &str) -> String {
        let kind = match kind {
            "qwen" => "Qwen",
            other => other,
        };
        match self.scope.sub_provider_parts() {
            Some(("alibaba", "international")) => format!("{kind} Alibaba International"),
            Some(("alibaba", "china")) => format!("{kind} Alibaba China"),
            Some((provider, variant)) => format!("{kind} {provider} {variant}"),
            None => kind.to_owned(),
        }
    }
}

/// Account-binding resolution for one managed launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedLaunchState {
    /// Final cwd, environment, model, and argv still need compilation.
    PendingResolution,
    /// This adapter has no exact managed-account selection.
    Unsupported,
    /// Exact selection applies, but current inputs prove no account.
    Unresolved,
    /// Current inputs prove one exact provider account.
    Bound(ProviderAccountBinding),
}

impl ManagedLaunchState {
    pub fn binding(&self) -> Option<&ProviderAccountBinding> {
        match self {
            Self::Bound(binding) => Some(binding),
            Self::PendingResolution | Self::Unsupported | Self::Unresolved => None,
        }
    }

    pub(crate) fn capacity(
        &self,
        runtime: &RuntimePaths,
        key: &LoginKey,
    ) -> Option<ProviderCapacity> {
        match self {
            Self::Unsupported => ProviderCapacity::read(runtime, key),
            Self::Bound(binding) => ProviderCapacity::read_bound(runtime, key, binding),
            Self::PendingResolution | Self::Unresolved => None,
        }
    }

    pub(crate) fn exact_account_applies(&self) -> bool {
        matches!(self, Self::Unresolved | Self::Bound(_))
    }
}

/// Cache identity of the credentials behind one provider usage probe.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountUsageIdentity {
    pub scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_stamp: Option<u64>,
}

impl AccountUsageIdentity {
    pub(super) fn binding(&self) -> Option<ProviderAccountBinding> {
        ProviderAccountBinding::new(self.scope.clone(), self.account_key.clone()?)
    }
}

/// Provider-account subscription capacity for one agent kind. Kind-wide
/// windows drive general harness policy; exact bindings opt a managed launch
/// into account-specific controls. Named and durationless quotas remain
/// display data in the cache.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderCapacity {
    pub(crate) windows: Vec<RateLimitWindow>,
    pacing_max_mins: Option<u32>,
}

impl ProviderCapacity {
    /// Read one kind-wide capacity from the shared provider cache.
    pub fn read(runtime: &RuntimePaths, key: &LoginKey) -> Option<Self> {
        let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
        let entry = cache.entries.get(&key.clone().into())?;
        entry.scope.is_kind_wide().then(|| Self {
            windows: entry.limits.windows.clone(),
            pacing_max_mins: None,
        })
    }

    /// Read capacity only when the cache belongs to this exact provider
    /// account. Bound Qwen pacing uses its sliding 5-hour/7-day windows while
    /// all authoritative windows remain available to the exhaustion gate.
    pub(crate) fn read_bound(
        runtime: &RuntimePaths,
        key: &LoginKey,
        binding: &ProviderAccountBinding,
    ) -> Option<Self> {
        let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
        let entry = cache
            .entries
            .get(&AccountUsageKey::new(key.clone(), binding.scope.clone()))?;
        entry_matches_binding(entry, binding).then(|| Self {
            windows: entry
                .bound_limits
                .as_ref()
                .unwrap_or(&entry.limits)
                .windows
                .clone(),
            pacing_max_mins: Some(7 * 24 * 60),
        })
    }

    /// Read all kind-wide capacities from the shared provider cache once.
    pub(crate) fn read_all(
        runtime: &RuntimePaths,
        logins: &RoomLoginSet,
    ) -> BTreeMap<LoginKey, Self> {
        let in_use = logins.keys_in_use();
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .into_iter()
            .filter(|(key, entry)| {
                key.scope.is_kind_wide()
                    && entry.scope.is_kind_wide()
                    && in_use.contains(&key.login)
            })
            .map(|(key, entry)| {
                (
                    key.login,
                    Self {
                        windows: entry.limits.windows,
                        pacing_max_mins: None,
                    },
                )
            })
            .collect()
    }

    pub(crate) fn from_windows(windows: Vec<RateLimitWindow>) -> Self {
        Self {
            windows,
            pacing_max_mins: None,
        }
    }

    pub(crate) fn projected_windows(
        &self,
        now: Timestamp,
    ) -> impl Iterator<Item = RateLimitWindow> + '_ {
        self.windows
            .iter()
            .cloned()
            .map(move |window| window.projected_at(now))
    }

    /// The unscoped window of `span`, projected at `now`.
    /// Model sub-caps share their parent's duration and never match; of two
    /// unscoped windows with one span, the last is read, as the longest-window
    /// choice reads it.
    pub fn window_of_span(&self, span: WindowSpan, now: Timestamp) -> Option<RateLimitWindow> {
        self.windows
            .iter()
            .rfind(|window| window.scope.is_none() && window.duration_mins == Some(span.minutes()))
            .cloned()
            .map(|window| window.projected_at(now))
    }

    /// Current observation of the longest duration-bearing window.
    pub(crate) fn longest_window_observation(&self, now: Timestamp) -> Option<RateLimitWindow> {
        Some(self.duration_window()?.clone().projected_at(now))
    }

    /// Latest reset among subscription windows spent now with a future reset.
    pub fn latest_spent_window_reset(&self, now: Timestamp) -> Option<Timestamp> {
        self.projected_duration_windows(now)
            .filter(|window| window.spent_with_future_reset(now))
            .filter_map(|window| window.resets_at)
            .max()
    }

    /// Whether every subscription window is known to be unused: it reads 0%,
    /// or its clock has not started, since a fresh window reports about 1%.
    /// A window without a percentage, or no window at all, is unknown rather
    /// than unused.
    pub(crate) fn known_unused(&self, now: Timestamp) -> bool {
        let mut windows = self.projected_duration_windows(now).peekable();
        windows.peek().is_some()
            && windows.all(|window| {
                window
                    .used_percentage
                    .is_some_and(|used| used == 0 || window.not_started(now))
            })
    }

    /// Latest reset of the account windows and this model's sub-caps.
    pub fn latest_spent_window_reset_for_model(
        &self,
        now: Timestamp,
        model: Option<&str>,
    ) -> Option<Timestamp> {
        self.model_windows(model)
            .filter(|window| window.is_spent())
            .filter_map(|window| window.resets_at.filter(|reset| *reset > now))
            .chain(self.latest_spent_window_reset(now))
            .max()
    }

    /// Model sub-caps can veto otherwise available account capacity.
    pub(crate) fn subscription_budget_available_for_model(
        &self,
        now: Timestamp,
        model: Option<&str>,
    ) -> bool {
        self.subscription_budget_available(now)
            && self
                .model_windows(model)
                .all(|window| !window_spent_unreset(window, now))
    }

    pub(crate) fn subscription_exhausted_for_model(
        &self,
        now: Timestamp,
        model: Option<&str>,
    ) -> bool {
        self.duration_windows()
            .any(|window| window_spent_unreset(window, now))
            || self.model_exhausted(now, model)
    }

    pub(crate) fn model_exhausted(&self, now: Timestamp, model: Option<&str>) -> bool {
        self.model_windows(model)
            .any(|window| window_spent_unreset(window, now))
    }

    fn model_windows(&self, model: Option<&str>) -> impl Iterator<Item = &RateLimitWindow> {
        let model = model.map(|model| {
            let id = super::all_definitions()
                .flat_map(|definition| definition.spec().launch.definitions.models)
                .find(|entry| entry.name == model)
                .map_or(model, |entry| entry.id);
            crate::agents::model_display::display_model(id).to_ascii_lowercase()
        });
        self.windows.iter().filter(move |window| {
            let Some(family) = window
                .scope
                .as_ref()
                .and_then(|scope| scope.id.strip_prefix("model:"))
            else {
                return false;
            };
            !family.is_empty()
                && window.duration_mins.is_some_and(|mins| mins > 0)
                && model
                    .as_deref()
                    .and_then(|model| model.strip_prefix(family))
                    .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with([' ', '-', '@']))
        })
    }

    /// Whether a known subscription reading currently has capacity.
    pub(crate) fn subscription_budget_available(&self, now: Timestamp) -> bool {
        let mut has_known_available = false;
        for window in self.projected_duration_windows(now) {
            if window_spent_unreset(&window, now) {
                return false;
            }
            has_known_available |= window.used_percentage.is_some() && !window.is_spent();
        }
        has_known_available
    }

    /// Forward budget headroom in the longest running window.
    pub(crate) fn longest_window_surplus(&self, now: Timestamp) -> Option<WindowSurplus> {
        let window = self.duration_window()?.clone().projected_at(now);
        let used_percentage = window.used_percentage?;
        let resets_at = window.resets_at?;
        let duration_mins = window.duration_mins.filter(|mins| *mins > 0)?;
        if resets_at <= now || window.not_started(now) {
            return None;
        }

        let duration_secs = f64::from(duration_mins) * 60.0;
        let until_reset_secs = resets_at.duration_since(now).as_secs_f64();
        let elapsed_secs = (duration_secs - until_reset_secs).max(0.0);
        let remaining_time_share = (until_reset_secs / duration_secs).clamp(f64::MIN_POSITIVE, 1.0);
        Some(WindowSurplus {
            duration_mins,
            elapsed: SignedDuration::from_secs(elapsed_secs as i64),
            headroom: (1.0 - f64::from(used_percentage) / 100.0) / remaining_time_share,
        })
    }

    /// Earliest future reset among currently exhausted authoritative windows.
    fn spent_window(&self, now: Timestamp) -> Option<RateLimitWindow> {
        self.windows
            .iter()
            .filter(|window| {
                window.scope.is_none()
                    && window.is_spent()
                    && window.resets_at.is_some_and(|reset| reset > now)
            })
            .min_by_key(|window| window.resets_at)
            .cloned()
    }

    fn duration_window(&self) -> Option<&RateLimitWindow> {
        self.duration_windows()
            .max_by_key(|window| window.duration_mins)
    }

    fn duration_windows(&self) -> impl Iterator<Item = &RateLimitWindow> {
        self.windows.iter().filter(|window| {
            window.scope.is_none()
                && window.duration_mins.is_some_and(|mins| {
                    mins > 0 && self.pacing_max_mins.is_none_or(|maximum| mins <= maximum)
                })
        })
    }

    fn projected_duration_windows(
        &self,
        now: Timestamp,
    ) -> impl Iterator<Item = RateLimitWindow> + '_ {
        self.duration_windows()
            .cloned()
            .map(move |window| window.projected_at(now))
    }
}

fn entry_matches_binding(entry: &RateLimitCacheEntry, binding: &ProviderAccountBinding) -> bool {
    &entry.scope == binding.scope() && entry.account_key.as_deref() == Some(binding.account_key())
}

#[doc(hidden)]
pub fn provider_budget_gate(
    runtime: &RuntimePaths,
    key: &LoginKey,
    binding: &ProviderAccountBinding,
    now: Timestamp,
) -> Option<String> {
    let window = ProviderCapacity::read_bound(runtime, key, binding)?.spent_window(now)?;
    let reset = window.resets_at?;
    let used_percentage = window.used_percentage.unwrap_or(100);
    let window_label = window
        .duration_mins
        .map(crate::theme::fmt::window_duration_label)
        .unwrap_or_else(|| "quota".to_owned());
    Some(format!(
        "{} {window_label} window exhausted ({used_percentage}% used); resets at {reset}",
        binding.display_label(key.kind.as_str()),
    ))
}

/// A subscription window length that loop triggers name: `5h` or `7d`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WindowSpan {
    FiveHour,
    SevenDay,
}

impl WindowSpan {
    pub const fn minutes(self) -> u32 {
        match self {
            Self::FiveHour => 5 * 60,
            Self::SevenDay => 7 * 24 * 60,
        }
    }
}

impl std::fmt::Display for WindowSpan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FiveHour => "5h",
            Self::SevenDay => "7d",
        })
    }
}

impl std::str::FromStr for WindowSpan {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "5h" => Ok(Self::FiveHour),
            "7d" => Ok(Self::SevenDay),
            other => Err(format!("unknown window span `{other}`; use 5h or 7d")),
        }
    }
}

/// Forward budget headroom in one provider's longest running window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowSurplus {
    pub duration_mins: u32,
    pub elapsed: SignedDuration,
    pub headroom: f64,
}

fn window_spent_unreset(window: &RateLimitWindow, now: Timestamp) -> bool {
    window.is_spent() && window.resets_at.is_none_or(|reset| reset > now)
}

/// Producer-published per-provider rate-limit windows.
const RATE_LIMITS_CACHE_VERSION: u32 = 7;

/// One credential's usage under a login; kind-wide keys keep the login's text.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountUsageKey {
    pub login: LoginKey,
    pub scope: ProviderAccountScope,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid usage key `{0}`; expected `<kind>@<name>[/<provider>/<variant>]`")]
pub struct InvalidAccountUsageKey(String);

impl AccountUsageKey {
    pub fn new(login: LoginKey, scope: ProviderAccountScope) -> Self {
        Self { login, scope }
    }

    pub(super) fn deserialize_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<Self, V>, D::Error>
    where
        D: serde::Deserializer<'de>,
        V: Deserialize<'de>,
    {
        let entries = BTreeMap::<String, V>::deserialize(deserializer)?;
        Ok(entries
            .into_iter()
            .filter_map(|(key, value)| Some((key.parse().ok()?, value)))
            .collect())
    }
}

impl From<LoginKey> for AccountUsageKey {
    fn from(login: LoginKey) -> Self {
        Self::new(login, ProviderAccountScope::KindWide)
    }
}

impl std::str::FromStr for AccountUsageKey {
    type Err = InvalidAccountUsageKey;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('/');
        let invalid = || InvalidAccountUsageKey(value.to_owned());
        let login = parts
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let scope = match (parts.next(), parts.next(), parts.next()) {
            (None, None, None) => ProviderAccountScope::KindWide,
            (Some(provider), Some(variant), None)
                if !provider.is_empty() && !variant.is_empty() =>
            {
                ProviderAccountScope::sub_provider(provider, variant)
            }
            _ => return Err(invalid()),
        };
        Ok(Self::new(login, scope))
    }
}

impl TryFrom<String> for AccountUsageKey {
    type Error = InvalidAccountUsageKey;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<AccountUsageKey> for String {
    fn from(value: AccountUsageKey) -> Self {
        value.to_string()
    }
}

impl std::fmt::Display for AccountUsageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.login)?;
        if let ProviderAccountScope::SubProvider { provider, variant } = &self.scope {
            write!(f, "/{provider}/{variant}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitsCache {
    pub version: u32,
    pub refreshed_at_ms: u64,
    #[serde(
        default,
        deserialize_with = "AccountUsageKey::deserialize_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub entries: BTreeMap<AccountUsageKey, RateLimitCacheEntry>,
}

impl Default for RateLimitsCache {
    fn default() -> Self {
        Self {
            version: RATE_LIMITS_CACHE_VERSION,
            refreshed_at_ms: 0,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitCacheEntry {
    /// Last early reset spotted in an authoritative account reading, in Unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_spotted_at_ms: Option<u64>,
    #[serde(default)]
    pub scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    #[serde(default)]
    pub limits: AgentRateLimits,
    /// Authoritative windows tied to `account_key` for exact-account controls.
    /// Display readers use fused `limits`; bound controls prefer this copy so a
    /// scope-only live reading cannot acquire the cached credential identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_limits: Option<AgentRateLimits>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingRefill>,
    /// When this credential's display first went unknown, marking the open episode so
    /// the authoritative refresh it forces runs once rather than every frame.
    /// Cleared the moment a usable window paints again, re-arming the next
    /// episode. Absent in caches written before the marker existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_since_ms: Option<u64>,
}

impl std::fmt::Debug for RateLimitCacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitCacheEntry")
            .field("reset_spotted_at_ms", &self.reset_spotted_at_ms)
            .field("scope", &self.scope)
            .field(
                "account_key",
                &self.account_key.as_ref().map(|_| "<redacted>"),
            )
            .field("limits", &self.limits)
            .field("bound_limits", &self.bound_limits)
            .field("pending", &self.pending)
            .field("unknown_since_ms", &self.unknown_since_ms)
            .finish()
    }
}

/// A best-effort refill awaiting confirmation by rate-limit fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRefill {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mins: Option<u32>,
    pub used_percentage: u8,
    pub first_seen_at: Timestamp,
}

impl PendingRefill {
    pub(crate) fn key(&self) -> RateLimitWindowKey {
        self.scope_id.as_ref().map_or_else(
            || RateLimitWindowKey::Duration(self.duration_mins),
            |scope| RateLimitWindowKey::Scope(scope.clone()),
        )
    }
}

/// Read the provider capacity cache, cold-dropping corrupt or unknown versions.
pub(crate) fn read_rate_limits_cache(path: &Path) -> RateLimitsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|cache: &RateLimitsCache| cache.version == RATE_LIMITS_CACHE_VERSION)
        .unwrap_or_default()
}

/// Latest cached window observation, falling back to the credits reading.
pub fn cached_usage_read_at(runtime: &RuntimePaths, key: &LoginKey) -> Option<Timestamp> {
    read_rate_limits_cache(&runtime.shared_rate_limits_path())
        .entries
        .into_iter()
        .filter(|(usage_key, _)| usage_key.login == *key)
        .flat_map(|(_, entry)| {
            entry
                .limits
                .windows
                .into_iter()
                .filter_map(|window| window.observed_at)
        })
        .max()
        .or_else(|| {
            let cache = crate::agents::credits::read_credits_cache(&runtime.shared_credits_path());
            cache
                .logins
                .get(key)
                .into_iter()
                .chain(
                    cache
                        .scoped
                        .iter()
                        .filter(|(usage_key, _)| usage_key.login == *key)
                        .map(|(_, entry)| entry),
                )
                .filter_map(|entry| i64::try_from(entry.observed_at_ms).ok())
                .filter_map(|ms| Timestamp::from_millisecond(ms).ok())
                .max()
        })
}

/// Best-effort credential-file mtime for provider usage ranking.
pub(super) fn file_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

/// The outcome of an out-of-band account probe. A settled `Found` or `LoggedOut`
/// answer rides the producer's long success TTL. A provisional `Found` retries
/// on the short TTL. `Unavailable` backs off from the short TTL to the success
/// TTL across consecutive failures, retaining the last-known account facts.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "account facts move directly into the cache without another allocation"
)]
pub enum AccountProbe {
    /// A logged-in account with the identity, plan, and metering facts its
    /// provider exposes.
    Found(super::AgentAccount),
    /// The probe ran and authoritatively found no login (logged out, or an auth
    /// file naming no credential). Cache it like a success: it changes about never.
    LoggedOut,
    /// The probe could not complete — the binary is missing, it exited non-zero,
    /// or its file was unreadable. Back off; absence here is not logged-out.
    Unavailable(AccountProbeError),
}

/// Secret-free cause of an unsuccessful account probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountProbeError {
    SpawnFailed,
    TimedOut,
    ExitNonZero(Option<i32>),
    UnparseableOutput,
    ReadFailed,
    SourceUnavailable,
}

impl std::fmt::Display for AccountProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed => f.write_str("could not start"),
            Self::TimedOut => f.write_str("timed out"),
            Self::ExitNonZero(Some(code)) => write!(f, "exited {code}"),
            Self::ExitNonZero(None) => f.write_str("terminated by signal"),
            Self::UnparseableOutput => f.write_str("unparseable output"),
            Self::ReadFailed => f.write_str("could not read credentials"),
            Self::SourceUnavailable => f.write_str("account source unavailable"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedemptionCode {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
    Unknown,
    Cooldown,
}

impl RedemptionCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::NothingToReset => "nothing_to_reset",
            Self::NoCredit => "no_credit",
            Self::AlreadyRedeemed => "already_redeemed",
            Self::Unknown => "unknown",
            Self::Cooldown => "cooldown",
        }
    }
}

pub struct PreparedRedemption<T> {
    pub decision: T,
    pub capacity: Option<ProviderCapacity>,
    pub credits: super::ResetCredits,
    offer: ResetCreditOffer,
}

impl<T> PreparedRedemption<T> {
    #[cfg(test)]
    pub(crate) fn from_offer(decision: T, offer: ResetCreditOffer) -> Self {
        Self {
            decision,
            capacity: offer.capacity.clone(),
            credits: offer.credits.clone(),
            offer,
        }
    }

    pub(crate) fn offer(&self) -> &ResetCreditOffer {
        &self.offer
    }

    pub(crate) fn hold(&self) -> Option<&RedeemHold> {
        self.offer.hold.as_ref()
    }

    /// Spend the prepared provider-owned credit only after the caller has
    /// durably recorded its attempt.
    pub fn consume(self, request_id: &str) -> Result<ResetCreditResult, String> {
        self.offer.consume(request_id)
    }
}

pub struct ResetCreditOffer {
    pub capacity: Option<ProviderCapacity>,
    pub credits: super::ResetCredits,
    pub extra_credits: Option<super::ExtraCredits>,
    pub hold: Option<RedeemHold>,
    action: Box<dyn ResetCreditAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedeemHold {
    pub code: RedemptionCode,
    pub reason: String,
}

impl ResetCreditOffer {
    pub fn new(
        capacity: Option<ProviderCapacity>,
        credits: super::ResetCredits,
        extra_credits: Option<super::ExtraCredits>,
        action: impl ResetCreditAction + 'static,
    ) -> Self {
        Self {
            capacity,
            credits,
            extra_credits,
            hold: None,
            action: Box::new(action),
        }
    }

    fn consume(self, request_id: &str) -> Result<ResetCreditResult, String> {
        self.action.consume(request_id)
    }
}

pub trait ResetCreditAction: Send {
    fn consume(self: Box<Self>, request_id: &str) -> Result<ResetCreditResult, String>;
}

pub struct ResetCreditResult {
    pub outcome: RedemptionCode,
    pub windows_reset: i64,
    pub refreshed: Option<(super::AccountUsageIdentity, super::AccountUsageSnapshot)>,
    pub refresh_error: Option<String>,
}

/// Prepare one provider-owned reset-credit action after a caller-supplied,
/// provider-neutral policy verdict. The caller owns the durable reservation
/// that must precede [`PreparedRedemption::consume`].
pub fn prepare_reset_credit_redemption<T>(
    kind: &str,
    decide: impl FnOnce(&ResetCreditOffer) -> Option<T>,
    login_env: &std::collections::BTreeMap<String, String>,
) -> Result<Option<PreparedRedemption<T>>, String> {
    let offer = super::find_definition(kind)
        .ok_or_else(|| format!("{kind} does not support reset-credit redemption"))?
        .prepare_reset_credit(login_env)?;
    let capacity = offer.capacity.clone();
    let credits = offer.credits.clone();
    let Some(decision) = decide(&offer) else {
        return Ok(None);
    };
    Ok(Some(PreparedRedemption {
        decision,
        capacity,
        credits,
        offer,
    }))
}
