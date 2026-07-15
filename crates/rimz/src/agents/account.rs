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

use super::{AgentRateLimits, ProviderAccountScope, RateLimitWindow, context::RateLimitWindowKey};
use crate::RuntimePaths;
use crate::ids::AgentKind;

/// Informational account and CLI-version probes are best-effort enrichment.
/// Bound every subprocess so one installed but wedged CLI cannot hold the
/// shared account cache producer indefinitely.
pub(crate) const INFORMATIONAL_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(test)]
mod tests;

/// Cache identity of the credentials behind one provider usage probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountUsageIdentity {
    pub scope: ProviderAccountScope,
    pub account_key: Option<String>,
    pub credentials_stamp: Option<u64>,
}

/// Provider-account subscription capacity for one agent kind. Only kind-wide
/// windows enter harness policy; named and durationless quotas remain display
/// data in the cache.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderCapacity {
    pub(crate) windows: Vec<RateLimitWindow>,
}

impl ProviderCapacity {
    /// Read one kind-wide capacity from the shared provider cache.
    pub fn read(runtime: &RuntimePaths, kind: &str) -> Option<Self> {
        let cache = read_rate_limits_cache(&runtime.shared_rate_limits_path());
        let entry = cache.entries.get(kind)?;
        entry.scope.is_kind_wide().then(|| Self {
            windows: entry.limits.windows.clone(),
        })
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
                    },
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn from_windows(windows: Vec<RateLimitWindow>) -> Self {
        Self { windows }
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
            .filter(|window| window_spent_unreset(window, now))
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

    /// Raw reset stamp for the longest duration-bearing window.
    pub(crate) fn longest_window_reset_at(&self) -> Option<Timestamp> {
        self.duration_window(true)?.resets_at
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
            window.scope.is_none() && window.duration_mins.is_some_and(|mins| mins > 0)
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
const RATE_LIMITS_CACHE_VERSION: u32 = 3;

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

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitCacheEntry {
    #[serde(default)]
    pub scope: ProviderAccountScope,
    #[serde(default)]
    pub limits: AgentRateLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PendingRefill>,
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
