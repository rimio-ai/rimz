//! Best-effort provider account probe contract.
//!
//! Account/plan facts are account-scoped, not session-scoped, and some never
//! ride the session context: Claude's subscription tier comes from `claude auth
//! status`, not its statusline. Each adapter probes those out-of-band facts in
//! its own `account.rs` ([`AgentAdapter::probe_account`]); this module owns the
//! shared [`AccountProbe`] outcome the sidebar producer folds onto the provider
//! dashboard.
//!
//! Producer-only: a probe may fork a subprocess, so the elected producer runs
//! it and publishes the result to the shared `accounts.json` cache (TTL'd,
//! single-flighted like the diff stats); consumer tabs read that cache and never
//! fork. A probe is a pure read — the cross-process memoization lives one layer
//! up, in [`crate::sidebar::cache`]'s producer cache.
//!
//! A probe also detects a *logged-in but idle* provider — one with no active
//! session this run — so the dashboard can show substantive accounts and their
//! budgets between turns. A live session's richer context still wins where both
//! exist.
//!
//! Best-effort by contract: a missing binary, a logged-out account, or
//! unparseable output yields no account. It never fails a snapshot — account is
//! enrichment, never correctness.
//!
//! [`AgentAdapter::probe_account`]: super::AgentAdapter::probe_account

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use super::{
    AgentRateLimits, ProviderAccountScope, RateLimitWindow,
    context::{FRESH_WINDOW_USAGE_FLOOR, RateLimitWindowKey},
};
use crate::RuntimePaths;
use crate::ids::AgentKind;

/// Informational account and CLI-version probes are best-effort enrichment.
/// Bound every subprocess so one installed but wedged CLI cannot hold the
/// shared account cache producer indefinitely.
pub(crate) const INFORMATIONAL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests;

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
    pub(crate) fn new(scope: ProviderAccountScope, account_key: String) -> Option<Self> {
        (!account_key.trim().is_empty()).then_some(Self { scope, account_key })
    }

    pub fn scope(&self) -> &ProviderAccountScope {
        &self.scope
    }

    pub(crate) fn account_key(&self) -> &str {
        &self.account_key
    }

    pub(crate) fn encode(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }

    #[doc(hidden)]
    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value)
            .ok()
            .filter(|binding: &Self| !binding.account_key.trim().is_empty())
    }

    pub(crate) fn display_label(&self, kind: &str) -> String {
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

/// Cache identity of the credentials behind one provider usage probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountUsageIdentity {
    pub scope: ProviderAccountScope,
    pub account_key: Option<String>,
    pub credentials_stamp: Option<u64>,
}

impl AccountUsageIdentity {
    pub(crate) fn binding(&self) -> Option<ProviderAccountBinding> {
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

/// Provider truth for whether the longest subscription window has a live
/// countdown edge. Harness scheduling maps this provider-owned verdict onto
/// its generic reset schedule signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LongestWindowSignal {
    At(Timestamp),
    ConfirmedDown,
    Unknown,
}

impl ProviderCapacity {
    /// Read one kind-wide capacity from the shared provider cache.
    pub fn read(runtime: &RuntimePaths, kind: &str) -> Option<Self> {
        let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
        let entry = cache.entries.get(kind)?;
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
        kind: &str,
        binding: &ProviderAccountBinding,
    ) -> Option<Self> {
        let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
        let entry = cache.entries.get(kind)?;
        entry_matches_binding(entry, binding).then(|| Self {
            windows: entry.limits.windows.clone(),
            pacing_max_mins: Some(7 * 24 * 60),
        })
    }

    pub(crate) fn binding_cache_matches(
        runtime: &RuntimePaths,
        kind: &str,
        binding: &ProviderAccountBinding,
    ) -> Option<bool> {
        let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
        Some(entry_matches_binding(cache.entries.get(kind)?, binding))
    }

    /// Read all kind-wide capacities from the shared provider cache once.
    pub(crate) fn read_all(runtime: &RuntimePaths) -> BTreeMap<AgentKind, Self> {
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .into_iter()
            .filter(|(_, entry)| entry.scope.is_kind_wide())
            .map(|(kind, entry)| {
                (
                    AgentKind::new_unchecked(kind),
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

    /// Latest reset among subscription windows spent now with a future reset.
    pub(crate) fn latest_spent_window_reset(&self, now: Timestamp) -> Option<Timestamp> {
        self.projected_duration_windows(now)
            .filter(|window| window.spent_with_future_reset(now))
            .filter_map(|window| window.resets_at)
            .max()
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

    /// Whether the shortest duration-bearing window is currently running.
    pub(crate) fn shortest_window_running(&self, now: Timestamp) -> Option<bool> {
        window_running_verdict(self.duration_window(false)?, now)
    }

    /// Whether the longest duration-bearing window is currently running.
    pub(crate) fn longest_window_running(&self, now: Timestamp) -> Option<bool> {
        window_running_verdict(self.duration_window(true)?, now)
    }

    /// Whether an authoritative reading says the longest known duration is not
    /// currently enforced, so a reset primer has no window to start.
    pub(crate) fn longest_window_lifted(&self) -> bool {
        self.duration_window(true)
            .is_some_and(|window| window.lifted)
    }

    /// Authoritative state of the longest duration-bearing window without
    /// projecting a passed reset into a synthetic future countdown.
    pub(crate) fn longest_window_signal(&self, now: Timestamp) -> LongestWindowSignal {
        let Some(window) = self.duration_window(true) else {
            return LongestWindowSignal::Unknown;
        };
        if window.lifted {
            return LongestWindowSignal::Unknown;
        }
        if window.not_started(now) {
            return if window.source.is_authoritative() {
                LongestWindowSignal::ConfirmedDown
            } else {
                LongestWindowSignal::Unknown
            };
        }
        if let Some(resets_at) = window.resets_at {
            return LongestWindowSignal::At(resets_at);
        }
        if window
            .used_percentage
            .is_some_and(|used| used <= FRESH_WINDOW_USAGE_FLOOR)
            && window.source.is_authoritative()
        {
            LongestWindowSignal::ConfirmedDown
        } else {
            LongestWindowSignal::Unknown
        }
    }

    /// Forward budget headroom in the longest running window.
    pub(crate) fn longest_window_surplus(&self, now: Timestamp) -> Option<WindowSurplus> {
        let window = self.duration_window(true)?.clone().projected_at(now);
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
    pub(crate) fn spent_window(&self, now: Timestamp) -> Option<RateLimitWindow> {
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

    fn duration_window(&self, longest: bool) -> Option<&RateLimitWindow> {
        let windows = self.duration_windows();
        if longest {
            windows.max_by_key(|window| window.duration_mins)
        } else {
            windows.min_by_key(|window| window.duration_mins)
        }
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
    kind: &str,
    binding: &ProviderAccountBinding,
    now: Timestamp,
) -> Option<String> {
    let window = ProviderCapacity::read_bound(runtime, kind, binding)?.spent_window(now)?;
    let reset = window.resets_at?;
    let used_percentage = window.used_percentage.unwrap_or(100);
    let window_label = window
        .duration_mins
        .map(window_duration_label)
        .unwrap_or_else(|| "quota".to_owned());
    Some(format!(
        "{} {window_label} window exhausted ({used_percentage}% used); resets at {reset}",
        binding.display_label(kind),
    ))
}

fn window_duration_label(mins: u32) -> String {
    if mins.is_multiple_of(24 * 60) {
        format!("{}d", mins / (24 * 60))
    } else if mins.is_multiple_of(60) {
        format!("{}h", mins / 60)
    } else {
        format!("{mins}m")
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

fn window_running_verdict(window: &RateLimitWindow, now: Timestamp) -> Option<bool> {
    let projected = window.clone().projected_at(now);
    projected.used_percentage?;
    if projected.not_started(now) {
        return Some(false);
    }
    match projected.resets_at {
        Some(reset) if reset > now => Some(true),
        _ => None,
    }
}

/// Producer-published per-provider rate-limit windows.
const RATE_LIMITS_CACHE_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitsCache {
    pub version: u32,
    pub refreshed_at_ms: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, RateLimitCacheEntry>,
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
    #[serde(default)]
    pub scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_key: Option<String>,
    #[serde(default)]
    pub limits: AgentRateLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingRefill>,
}

impl std::fmt::Debug for RateLimitCacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitCacheEntry")
            .field("scope", &self.scope)
            .field(
                "account_key",
                &self.account_key.as_ref().map(|_| "<redacted>"),
            )
            .field("limits", &self.limits)
            .field("pending", &self.pending)
            .finish()
    }
}

/// A best-effort drop awaiting confirmation by rate-limit fusion.
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

/// Best-effort credential-file mtime for provider usage ranking.
pub(crate) fn file_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

/// The outcome of an out-of-band account probe. The three arms drive the
/// producer's cache TTL: a `Found` or `LoggedOut` answer is authoritative and
/// rides the long success TTL, while `Unavailable` — a binary that would not run,
/// a non-zero exit, an unreadable file — is a transient failure the producer
/// retries on the short failure TTL instead of pinning the dashboard empty for
/// the full success window.
#[derive(Debug)]
pub enum AccountProbe {
    /// A logged-in account with the identity, plan, and metering facts its
    /// provider exposes.
    Found(super::AgentAccount),
    /// The probe ran and authoritatively found no login (logged out, or an auth
    /// file naming no credential). Cache it like a success: it changes about never.
    LoggedOut,
    /// The probe could not complete — the binary is missing, it exited non-zero,
    /// or its file was unreadable. Retry soon; absence here is not logged-out.
    Unavailable,
}
