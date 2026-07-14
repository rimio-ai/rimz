use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::{
    AccountUsageIdentity, AccountUsageProbe, AccountUsageSnapshot, ExtraCredits,
    ProviderAccountScope, ResetCredits,
};
use crate::config::AccountsConfig;
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{
    ACCOUNT_USAGE_CLAIM_TTL, CREDITS_DISPLAY_MAX_AGE, OAUTH_USAGE_SETTLED_TTL, OAUTH_USAGE_TTL,
};
use crate::store::snapshot::format_plan_label;
use crate::{RuntimePaths, SidebarSnapshot};

/// Shared provider extra-credits cache, keyed by agent kind.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CreditsCache {
    pub refreshed_at_ms: u64,
    pub entries: BTreeMap<String, ProviderCreditsEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCreditsEntry {
    #[serde(default)]
    pub scope: ProviderAccountScope,
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
pub struct DirectQueryClaim {
    pub nonce: Uuid,
    pub claimed_at_ms: u64,
    pub requested_scope: ProviderAccountScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_stamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight_account_key: Option<String>,
}

#[derive(Debug)]
pub struct AccountUsageCompletion {
    pub identity: AccountUsageIdentity,
    pub snapshot: Option<AccountUsageSnapshot>,
    pub account_changed: bool,
}

pub(crate) fn read_credits_cache(path: &Path) -> CreditsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn write_credits_cache(path: &Path, cache: &CreditsCache) {
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(path, cache) {
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
    merge_provider_realtime_usage(
        runtime,
        kind,
        ProviderAccountScope::KindWide,
        AccountUsageSnapshot {
            extra_credits,
            ..Default::default()
        },
    );
}

pub fn merge_provider_realtime_usage(
    runtime: &RuntimePaths,
    kind: &str,
    scope: ProviderAccountScope,
    snapshot: AccountUsageSnapshot,
) {
    let (plan, extra_credits, reset_credits) = account_usage_credit_fields(Some(&snapshot));
    merge_provider_credits_entry(
        runtime,
        kind,
        ProviderCreditsEntry {
            scope,
            observed_at_ms: unix_now_ms(),
            plan: plan.clone(),
            ok: plan.is_some() || extra_credits.is_some() || reset_credits.is_some(),
            extra_credits,
            reset_credits,
            ..Default::default()
        },
    );
}

/// Claim one due direct account-usage read under the shared credits lock.
pub fn claim_provider_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    identity: AccountUsageIdentity,
) -> Option<Uuid> {
    claim_provider_account_usage_at(runtime, kind, identity, unix_now_ms(), Uuid::now_v7())
}

fn claim_provider_account_usage_at(
    runtime: &RuntimePaths,
    kind: &str,
    identity: AccountUsageIdentity,
    now_ms: u64,
    nonce: Uuid,
) -> Option<Uuid> {
    let path = runtime.shared_credits_path();
    let _guard = try_credits_cache_lock(&runtime.shared_credits_lock())?;
    let mut cache = read_credits_cache(&path);
    let entry = cache.entries.entry(kind.to_owned()).or_default();
    if entry.direct_query_claim.as_ref().is_some_and(|claim| {
        now_ms.saturating_sub(claim.claimed_at_ms) <= ACCOUNT_USAGE_CLAIM_TTL.as_millis() as u64
    }) || oauth_read_is_fresh(entry, now_ms, &identity)
    {
        return None;
    }
    entry.direct_query_claim = Some(DirectQueryClaim {
        nonce,
        claimed_at_ms: now_ms,
        requested_scope: identity.scope,
        credentials_stamp: identity.credentials_stamp,
        preflight_account_key: identity.account_key,
    });
    cache.refreshed_at_ms = now_ms;
    write_credits_cache(&path, &cache);
    Some(nonce)
}

pub fn account_usage_claim_matches(runtime: &RuntimePaths, kind: &str, nonce: Uuid) -> bool {
    read_credits_cache(&runtime.shared_credits_path())
        .entries
        .get(kind)
        .and_then(|entry| entry.direct_query_claim.as_ref())
        .is_some_and(|claim| claim.nonce == nonce)
}

pub fn cancel_provider_account_usage_claim(
    runtime: &RuntimePaths,
    kind: &str,
    nonce: Uuid,
) -> bool {
    let path = runtime.shared_credits_path();
    let Some(_guard) = try_credits_cache_lock(&runtime.shared_credits_lock()) else {
        return false;
    };
    let mut cache = read_credits_cache(&path);
    let Some(entry) = cache.entries.get_mut(kind) else {
        return false;
    };
    if entry.direct_query_claim.as_ref().map(|claim| claim.nonce) != Some(nonce) {
        return false;
    }
    entry.direct_query_claim = None;
    cache.refreshed_at_ms = unix_now_ms();
    write_credits_cache(&path, &cache);
    true
}

pub fn complete_provider_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    nonce: Uuid,
    probe: AccountUsageProbe,
) -> Option<AccountUsageCompletion> {
    let path = runtime.shared_credits_path();
    let _guard = try_credits_cache_lock(&runtime.shared_credits_lock())?;
    let mut cache = read_credits_cache(&path);
    let prior = cache.entries.get(kind).cloned()?;
    let claim = prior
        .direct_query_claim
        .as_ref()
        .filter(|claim| claim.nonce == nonce)?;
    let claim_identity = AccountUsageIdentity {
        scope: claim.requested_scope.clone(),
        account_key: claim.preflight_account_key.clone(),
        credentials_stamp: claim.credentials_stamp,
    };
    let (mut identity, snapshot, auth_settled, failed) = match probe {
        AccountUsageProbe::Found { identity, snapshot } => (identity, Some(snapshot), false, false),
        AccountUsageProbe::NoCredentials(identity) => (identity, None, true, false),
        AccountUsageProbe::Failed(identity) => (identity, None, false, true),
        AccountUsageProbe::Unsupported => (claim_identity.clone(), None, false, true),
    };
    if failed {
        if identity.scope == ProviderAccountScope::KindWide
            && claim_identity.scope != ProviderAccountScope::KindWide
        {
            identity.scope = claim_identity.scope;
        }
        identity.account_key = identity.account_key.or(claim_identity.account_key);
        identity.credentials_stamp = identity
            .credentials_stamp
            .or(claim_identity.credentials_stamp);
    }
    let account_changed = if failed {
        (identity.scope != ProviderAccountScope::KindWide && prior.scope != identity.scope)
            || identity
                .account_key
                .as_deref()
                .is_some_and(|current| prior.account_key.as_deref() != Some(current))
    } else {
        prior.scope != identity.scope
            || prior.account_key.as_deref() != identity.account_key.as_deref()
    };
    let now_ms = unix_now_ms();
    let (plan, extra_credits, reset_credits) = account_usage_credit_fields(snapshot.as_ref());
    let mut entry = ProviderCreditsEntry {
        scope: identity.scope.clone(),
        observed_at_ms: now_ms,
        oauth_read_at_ms: now_ms,
        auth_settled,
        credentials_stamp: identity.credentials_stamp,
        account_key: identity.account_key.clone(),
        ok: snapshot.is_some(),
        plan,
        extra_credits,
        reset_credits,
        direct_query_claim: None,
    };
    if !account_changed {
        if snapshot.is_none() {
            entry.observed_at_ms = prior.observed_at_ms;
            entry.ok = prior.ok;
            entry.plan = prior.plan;
            entry.extra_credits = prior.extra_credits;
            entry.reset_credits = prior.reset_credits;
            if failed {
                if entry.scope == ProviderAccountScope::KindWide {
                    entry.scope = prior.scope;
                }
                entry.credentials_stamp = entry.credentials_stamp.or(prior.credentials_stamp);
                entry.account_key = entry.account_key.or(prior.account_key);
            }
        } else {
            entry.plan = entry.plan.or(prior.plan);
            entry.extra_credits = entry.extra_credits.or(prior.extra_credits);
            entry.reset_credits = entry.reset_credits.or(prior.reset_credits);
        }
    }
    cache.refreshed_at_ms = now_ms;
    cache.entries.insert(kind.to_owned(), entry);
    write_credits_cache(&path, &cache);
    Some(AccountUsageCompletion {
        identity,
        snapshot,
        account_changed,
    })
}

fn account_usage_credit_fields(
    snapshot: Option<&AccountUsageSnapshot>,
) -> (Option<String>, Option<ExtraCredits>, Option<ResetCredits>) {
    let plan = snapshot
        .and_then(|usage| usage.plan.as_deref())
        .and_then(crate::agents::non_empty_trimmed);
    let extra_credits = snapshot.and_then(|usage| usage.extra_credits.clone());
    let reset_credits = snapshot.and_then(|usage| usage.reset_credits.clone());
    (plan, extra_credits, reset_credits)
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
    entry.plan = entry
        .plan
        .as_deref()
        .and_then(crate::agents::non_empty_trimmed);
    let prior = cache
        .entries
        .get(kind)
        .filter(|prior| prior.scope == entry.scope);
    if entry.oauth_read_at_ms == 0
        && let Some(prior) = prior
    {
        entry.oauth_read_at_ms = prior.oauth_read_at_ms;
        entry.auth_settled = prior.auth_settled;
        entry.credentials_stamp = prior.credentials_stamp;
        entry.account_key = prior.account_key.clone();
        entry.direct_query_claim = prior.direct_query_claim.clone();
        if entry.plan.is_none() {
            entry.plan = prior.plan.clone();
        }
    }
    if entry.extra_credits.is_none() {
        entry.extra_credits = prior.and_then(|entry| entry.extra_credits.clone());
    }
    if entry.reset_credits.is_none() {
        // A reading without reset-credit data must not erase the last successful
        // app-server or OAuth reset-credit read.
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
    entry.auth_settled = false;
    entry.direct_query_claim = None;
    cache.refreshed_at_ms = unix_now_ms();
    write_credits_cache(&path, &cache);
}

fn oauth_read_is_fresh(
    entry: &ProviderCreditsEntry,
    now_ms: u64,
    identity: &AccountUsageIdentity,
) -> bool {
    if entry.oauth_read_at_ms == 0 {
        return false;
    }
    if entry.scope != identity.scope
        || preflight_account_key_changed(entry, identity)
        || identity
            .credentials_stamp
            .is_some_and(|stamp| entry.credentials_stamp != Some(stamp))
    {
        return false;
    }
    let ttl = if entry.auth_settled {
        if entry.credentials_stamp != identity.credentials_stamp {
            return false;
        }
        OAUTH_USAGE_SETTLED_TTL
    } else {
        OAUTH_USAGE_TTL
    };
    now_ms.saturating_sub(entry.oauth_read_at_ms) <= ttl.as_millis() as u64
}

fn preflight_account_key_changed(
    entry: &ProviderCreditsEntry,
    identity: &AccountUsageIdentity,
) -> bool {
    match identity.account_key.as_deref() {
        Some(current) => entry.account_key.as_deref() != Some(current),
        // File-stamped scheduling hints omit owner to avoid rereading
        // credentials every tick. Without a stamp, `None` is the exact source
        // result and detects Some -> None symmetrically.
        None => identity.credentials_stamp.is_none() && entry.account_key.is_some(),
    }
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
            let displayable_entry = cache
                .entries
                .get(&panel.kind)
                .filter(|entry| entry.scope == panel.account_scope)
                .filter(|entry| entry_is_displayable(entry, now_ms));
            if panel.plan.is_none() {
                panel.plan = displayable_entry
                    .and_then(|entry| entry.plan.as_deref())
                    .filter(|plan| !plan.trim().is_empty())
                    .map(|plan| format_plan_label(&panel.kind, plan));
            }
            panel.extra_credits = displayable_entry
                .and_then(|entry| entry.extra_credits.clone())
                .map(|credits| credits.with_limit_if_missing(ceiling))
                .or_else(|| ceiling.map(|limit| ExtraCredits::known(None, None, Some(limit))));
            panel.reset_credits = displayable_entry.and_then(|entry| entry.reset_credits.clone());
            continue;
        }

        let used = panel.spending.as_ref().map(|spending| spending.month.usd);
        panel.extra_credits = Some(ExtraCredits::known(used, None, ceiling));
        panel.reset_credits = None;
    }
}

#[cfg(test)]
mod tests;
