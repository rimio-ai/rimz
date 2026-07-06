use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::{ExtraCredits, ResetCredits};
use crate::config::AccountsConfig;
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{CREDITS_DISPLAY_MAX_AGE, OAUTH_USAGE_SETTLED_TTL, OAUTH_USAGE_TTL};
use crate::{RuntimePaths, SidebarSnapshot};

/// Shared provider extra-credits cache, keyed by agent kind.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CreditsCache {
    pub refreshed_at_ms: u64,
    pub entries: BTreeMap<String, ProviderCreditsEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCreditsEntry {
    pub observed_at_ms: u64,
    /// Last OAuth account-usage attempt. App-server/realtime credit writes
    /// preserve this stamp and its settled-auth state and never advance the
    /// OAuth cadence.
    #[serde(default)]
    pub oauth_read_at_ms: u64,
    /// Last OAuth attempt settled as an auth failure: retried on the long TTL
    /// until the credential source changes.
    #[serde(default)]
    pub auth_settled: bool,
    /// Credential-source stamp at that settled attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_stamp: Option<u64>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ResetCredits>,
}

pub(crate) fn read_credits_cache(path: &Path) -> CreditsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn write_credits_cache(path: &Path, cache: &CreditsCache) {
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.credits_write",
            error = &err as &dyn std::error::Error,
            "sidebar credits cache write failed",
        );
    }
}

/// Merge one provider's paid-usage reading into the shared cache. Best-effort:
/// another producer may win the lock first, and the next due helper converges.
pub fn merge_provider_credits(
    runtime: &RuntimePaths,
    kind: &str,
    extra_credits: Option<ExtraCredits>,
) {
    merge_provider_credits_entry(
        runtime,
        kind,
        ProviderCreditsEntry {
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: 0,
            auth_settled: false,
            credentials_stamp: None,
            ok: extra_credits.is_some(),
            extra_credits,
            reset_credits: None,
        },
    );
}

/// Merge one provider's entry only when the existing success/failure entry is
/// past its TTL or a settled auth failure's credential source changed. The
/// freshness check happens under the shared lock, so detached helpers in
/// multiple workspaces single-flight the network fetch.
pub fn merge_provider_credits_entry_if_due(
    runtime: &RuntimePaths,
    kind: &str,
    current_stamp: Option<u64>,
    entry: impl FnOnce() -> ProviderCreditsEntry,
) -> Option<ProviderCreditsEntry> {
    let path = runtime.shared_credits_path();
    let _guard = try_credits_cache_lock(&runtime.shared_credits_lock())?;
    let mut cache = read_credits_cache(&path);
    let now_ms = unix_now_ms();
    let prior = cache.entries.get(kind).cloned();
    if prior
        .as_ref()
        .is_some_and(|entry| oauth_read_is_fresh(entry, now_ms, current_stamp))
    {
        return None;
    }
    let mut entry = entry();
    if entry.oauth_read_at_ms == 0 {
        entry.oauth_read_at_ms = unix_now_ms();
    }
    if !entry.ok {
        if let Some(prior) = prior.as_ref() {
            entry.observed_at_ms = prior.observed_at_ms;
            entry.ok = prior.ok;
            entry.extra_credits = prior.extra_credits.clone();
            entry.reset_credits = prior.reset_credits.clone();
        }
    } else if entry.reset_credits.is_none() {
        // Reset credits are Codex OAuth-only; app-server, extra-credits-only,
        // and failed writes must not erase the last successful reset read.
        entry.reset_credits = prior.as_ref().and_then(|entry| entry.reset_credits.clone());
    }
    cache.refreshed_at_ms = unix_now_ms();
    cache.entries.insert(kind.to_owned(), entry.clone());
    write_credits_cache(&path, &cache);
    Some(entry)
}

pub(crate) fn merge_provider_credits_entry(
    runtime: &RuntimePaths,
    kind: &str,
    entry: ProviderCreditsEntry,
) {
    let path = runtime.shared_credits_path();
    let Some(_guard) = try_credits_cache_lock(&runtime.shared_credits_lock()) else {
        return;
    };
    let mut cache = read_credits_cache(&path);
    let mut entry = entry;
    let prior = cache.entries.get(kind);
    if entry.oauth_read_at_ms == 0
        && let Some(prior) = prior
    {
        entry.oauth_read_at_ms = prior.oauth_read_at_ms;
        entry.auth_settled = prior.auth_settled;
        entry.credentials_stamp = prior.credentials_stamp;
    }
    if entry.reset_credits.is_none() {
        // Reset credits are Codex OAuth-only; app-server, extra-credits-only,
        // and failed writes must not erase the last successful reset read.
        entry.reset_credits = prior.and_then(|entry| entry.reset_credits.clone());
    }
    cache.refreshed_at_ms = unix_now_ms();
    cache.entries.insert(kind.to_owned(), entry);
    write_credits_cache(&path, &cache);
}

pub fn invalidate_oauth_read(runtime: &RuntimePaths, kind: &str) {
    let path = runtime.shared_credits_path();
    let Some(_guard) = try_credits_cache_lock(&runtime.shared_credits_lock()) else {
        return;
    };
    let mut cache = read_credits_cache(&path);
    let Some(entry) = cache.entries.get_mut(kind) else {
        return;
    };
    entry.oauth_read_at_ms = 0;
    cache.refreshed_at_ms = unix_now_ms();
    write_credits_cache(&path, &cache);
}

fn oauth_read_is_fresh(
    entry: &ProviderCreditsEntry,
    now_ms: u64,
    current_stamp: Option<u64>,
) -> bool {
    if entry.oauth_read_at_ms == 0 {
        return false;
    }
    let ttl = if entry.auth_settled {
        if entry.credentials_stamp != current_stamp {
            return false;
        }
        OAUTH_USAGE_SETTLED_TTL
    } else {
        OAUTH_USAGE_TTL
    };
    now_ms.saturating_sub(entry.oauth_read_at_ms) <= ttl.as_millis() as u64
}

fn entry_is_displayable(entry: &ProviderCreditsEntry, now_ms: u64) -> bool {
    entry.ok
        && now_ms.saturating_sub(entry.observed_at_ms) <= CREDITS_DISPLAY_MAX_AGE.as_millis() as u64
}

fn try_credits_cache_lock(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

pub(crate) fn apply_credits_cache(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    accounts: &AccountsConfig,
) {
    if snapshot.providers.is_empty() {
        return;
    }
    let cache = read_credits_cache(&runtime.shared_credits_path());
    apply_credits_cache_with(snapshot, &cache, accounts, unix_now_ms());
}

fn apply_credits_cache_with(
    snapshot: &mut SidebarSnapshot,
    cache: &CreditsCache,
    accounts: &AccountsConfig,
    now_ms: u64,
) {
    for panel in &mut snapshot.providers {
        let ceiling = accounts.usage_limit(&panel.kind);
        if panel.metered {
            panel.extra_credits = cache
                .entries
                .get(&panel.kind)
                .filter(|entry| entry_is_displayable(entry, now_ms))
                .and_then(|entry| entry.extra_credits.clone())
                .map(|credits| credits.with_limit_if_missing(ceiling))
                .or_else(|| ceiling.map(|limit| ExtraCredits::known(None, None, Some(limit))));
            panel.reset_credits = cache
                .entries
                .get(&panel.kind)
                .filter(|entry| entry_is_displayable(entry, now_ms))
                .and_then(|entry| entry.reset_credits.clone());
            continue;
        }

        let used = panel.spending.as_ref().map(|spending| spending.month.usd);
        panel.extra_credits = Some(ExtraCredits::known(used, None, ceiling));
        panel.reset_credits = None;
    }
}

#[cfg(test)]
mod tests;
