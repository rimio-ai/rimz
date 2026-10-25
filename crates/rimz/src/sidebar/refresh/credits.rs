//! Shared provider credits cache and pure nonce-gated usage completion.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use uuid::Uuid;

use crate::RuntimePaths;
use crate::agents::RoomLoginSet;
#[cfg(test)]
use crate::agents::credits::CREDITS_DISPLAY_MAX_AGE;
pub(super) use crate::agents::credits::{
    CreditsCache, DirectQueryClaim, ProviderCreditsEntry, entry_is_displayable, read_credits_cache,
};
use crate::agents::{
    AccountUsageIdentity, AccountUsageProbe, AccountUsageSnapshot, Entitlement, ExtraCredits,
    ProviderAccountScope, ResetCredits,
};
use crate::config::AccountsConfig;
use crate::ids::LoginKey;
use crate::sidebar::timing::{
    ACCOUNT_USAGE_CLAIM_TTL, IDLE_OAUTH_USAGE_TTL, OAUTH_USAGE_SETTLED_TTL, OAUTH_USAGE_TTL,
};
use crate::store::snapshot::{SidebarSnapshot, format_plan_label};
use crate::utils::time::unix_now_ms;

impl ProviderCreditsEntry {
    pub(super) fn invalidate_oauth_read(&mut self) {
        self.oauth_read_at_ms = 0;
        self.oauth_read_hint = None;
        self.auth_settled = false;
        self.direct_query_claim = None;
    }

    /// Complete one matching account-usage claim without I/O or clock reads.
    pub(super) fn complete_account_usage(
        self,
        nonce: Uuid,
        probe: AccountUsageProbe,
        now_ms: u64,
    ) -> Option<(Self, AccountUsageCompletion)> {
        let claim = self
            .direct_query_claim
            .as_ref()
            .filter(|claim| claim.nonce == nonce)?;
        let claim_identity = AccountUsageIdentity {
            scope: claim.requested_scope.clone(),
            account_key: claim.preflight_account_key.clone(),
            credentials_stamp: claim.credentials_stamp,
        };
        let not_entitled = matches!(&probe, AccountUsageProbe::NotEntitled(_));
        let NormalizedProbe {
            identity,
            snapshot,
            auth_settled,
            failed,
        } = normalize_probe(probe, claim_identity);
        let account_changed = account_identity_changed(&self, &identity, failed);
        let mut entitlement = if snapshot.is_some()
            || (account_changed && (self.account_key.is_some() || self.observed_at_ms != 0))
        {
            Entitlement::Ok
        } else {
            self.entitlement
        };
        if not_entitled && entitlement == Entitlement::Ok {
            entitlement = Entitlement::Lapsed { since_ms: now_ms };
        }
        let (plan, extra_credits, reset_credits) = account_usage_credit_fields(snapshot.as_ref());
        let mut next = Self {
            scope: identity.scope.clone(),
            entitlement,
            observed_at_ms: now_ms,
            oauth_read_at_ms: now_ms,
            oauth_read_hint: claim.hint.clone(),
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
                next.observed_at_ms = self.observed_at_ms;
                next.ok = self.ok;
                next.plan = self.plan.clone();
                next.extra_credits = self.extra_credits.clone();
                next.reset_credits = self.reset_credits.clone();
                if failed {
                    if next.scope == ProviderAccountScope::KindWide {
                        next.scope = self.scope.clone();
                    }
                    next.credentials_stamp = next.credentials_stamp.or(self.credentials_stamp);
                    next.account_key = next.account_key.or_else(|| self.account_key.clone());
                }
            } else {
                fill_missing_display_fields(&mut next, &self);
            }
        }
        Some((
            next,
            AccountUsageCompletion {
                identity,
                snapshot,
                account_changed,
            },
        ))
    }
}

struct NormalizedProbe {
    identity: AccountUsageIdentity,
    snapshot: Option<AccountUsageSnapshot>,
    auth_settled: bool,
    failed: bool,
}

fn normalize_probe(
    probe: AccountUsageProbe,
    claim_identity: AccountUsageIdentity,
) -> NormalizedProbe {
    let (mut identity, snapshot, auth_settled, failed) = match probe {
        AccountUsageProbe::Found { identity, snapshot } => (identity, Some(snapshot), false, false),
        AccountUsageProbe::NoCredentials(identity) => (identity, None, true, false),
        AccountUsageProbe::NotEntitled(identity) => (identity, None, true, false),
        AccountUsageProbe::Failed(identity) => (identity, None, false, true),
        AccountUsageProbe::Unsupported => (claim_identity.clone(), None, false, true),
    };
    // A delegated probe can fail before selecting its sub-provider. Preserve
    // the claimed identity so display carryover and panel scope filtering
    // still describe the account the probe was asked to read.
    if failed || auth_settled {
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
    NormalizedProbe {
        identity,
        snapshot,
        auth_settled,
        failed,
    }
}

fn account_identity_changed(
    prior: &ProviderCreditsEntry,
    identity: &AccountUsageIdentity,
    failed: bool,
) -> bool {
    if failed {
        (identity.scope != ProviderAccountScope::KindWide && prior.scope != identity.scope)
            || identity
                .account_key
                .as_deref()
                .is_some_and(|current| prior.account_key.as_deref() != Some(current))
    } else {
        prior.scope != identity.scope
            || prior.account_key.as_deref() != identity.account_key.as_deref()
    }
}

fn fill_missing_display_fields(entry: &mut ProviderCreditsEntry, prior: &ProviderCreditsEntry) {
    entry.plan = entry.plan.take().or_else(|| prior.plan.clone());
    entry.extra_credits = entry
        .extra_credits
        .take()
        .or_else(|| prior.extra_credits.clone());
    entry.reset_credits = entry
        .reset_credits
        .take()
        .or_else(|| prior.reset_credits.clone());
}

#[derive(Debug)]
pub(super) struct AccountUsageCompletion {
    pub identity: AccountUsageIdentity,
    pub snapshot: Option<AccountUsageSnapshot>,
    pub account_changed: bool,
}

pub(super) fn write_credits_cache(path: &Path, cache: &CreditsCache) {
    if let Err(err) = crate::disk::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.credits_write",
            error = &err as &dyn std::error::Error,
            "sidebar credits cache write failed",
        );
    }
}

pub(super) fn merge_provider_realtime_usage(
    runtime: &RuntimePaths,
    key: &LoginKey,
    identity: AccountUsageIdentity,
    snapshot: AccountUsageSnapshot,
) {
    let (plan, extra_credits, reset_credits) = account_usage_credit_fields(Some(&snapshot));
    let mut entry = ProviderCreditsEntry {
        scope: identity.scope,
        account_key: identity.account_key,
        credentials_stamp: identity.credentials_stamp,
        observed_at_ms: unix_now_ms(),
        ok: plan.is_some() || extra_credits.is_some() || reset_credits.is_some(),
        plan,
        extra_credits,
        reset_credits,
        ..Default::default()
    };
    let path = runtime.shared_credits_path();
    let Some(_guard) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_credits_lock())
            .ok()
            .flatten()
    else {
        return;
    };
    let mut cache = read_credits_cache(&path);
    if let Some(prior) = cache
        .logins
        .get(key)
        .filter(|prior| prior.scope == entry.scope)
    {
        entry.direct_query_claim = prior.direct_query_claim.clone();
        if prior.account_key == entry.account_key {
            entry.entitlement = prior.entitlement;
            fill_missing_display_fields(&mut entry, prior);
            // The last direct read describes the credentials it saw; new ones
            // leave the entry unread, so the next claim re-reads them.
            if prior.credentials_stamp == entry.credentials_stamp {
                entry.oauth_read_at_ms = prior.oauth_read_at_ms;
                entry.oauth_read_hint = prior.oauth_read_hint.clone();
                entry.auth_settled = prior.auth_settled;
            }
        }
    }
    cache.refreshed_at_ms = unix_now_ms();
    cache.logins.insert(key.to_owned(), entry);
    write_credits_cache(&path, &cache);
}

/// Claim one due direct account-usage read under the shared credits lock.
pub(super) fn claim_provider_account_usage(
    runtime: &RuntimePaths,
    key: &LoginKey,
    cached_hint: Option<AccountUsageIdentity>,
) -> Option<Uuid> {
    claim_provider_account_usage_with_hint_at(
        runtime,
        key,
        cached_hint,
        unix_now_ms(),
        Uuid::now_v7(),
    )
}

fn claim_provider_account_usage_with_hint_at(
    runtime: &RuntimePaths,
    key: &LoginKey,
    cached_hint: Option<AccountUsageIdentity>,
    now_ms: u64,
    nonce: Uuid,
) -> Option<Uuid> {
    claim_provider_account_usage_locked(runtime, key, now_ms, nonce, Duration::ZERO, cached_hint)
}

/// Claim one due direct read of an idle account: the ordinary claim, held to
/// `IDLE_OAUTH_USAGE_TTL` since the last attempt of any room and any outcome.
pub(super) fn claim_idle_account_usage(
    runtime: &RuntimePaths,
    key: &LoginKey,
    cached_hint: Option<AccountUsageIdentity>,
) -> Option<Uuid> {
    claim_idle_account_usage_at(runtime, key, cached_hint, unix_now_ms(), Uuid::now_v7())
}

fn claim_idle_account_usage_at(
    runtime: &RuntimePaths,
    key: &LoginKey,
    cached_hint: Option<AccountUsageIdentity>,
    now_ms: u64,
    nonce: Uuid,
) -> Option<Uuid> {
    claim_provider_account_usage_locked(
        runtime,
        key,
        now_ms,
        nonce,
        IDLE_OAUTH_USAGE_TTL,
        cached_hint,
    )
}

#[cfg(test)]
fn claim_provider_account_usage_at(
    runtime: &RuntimePaths,
    key: &LoginKey,
    identity: AccountUsageIdentity,
    now_ms: u64,
    nonce: Uuid,
) -> Option<Uuid> {
    claim_provider_account_usage_with_hint_at(runtime, key, Some(identity), now_ms, nonce)
}

fn claim_provider_account_usage_locked(
    runtime: &RuntimePaths,
    key: &LoginKey,
    now_ms: u64,
    nonce: Uuid,
    floor: Duration,
    cached_hint: Option<AccountUsageIdentity>,
) -> Option<Uuid> {
    let path = runtime.shared_credits_path();
    let _guard = crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_credits_lock())
        .ok()
        .flatten()?;
    let mut cache = read_credits_cache(&path);
    let entry = cache.logins.entry(key.to_owned()).or_default();
    let inside_floor = entry.oauth_read_at_ms != 0
        && now_ms.saturating_sub(entry.oauth_read_at_ms) < floor.as_millis() as u64;
    if entry.direct_query_claim.as_ref().is_some_and(|claim| {
        now_ms.saturating_sub(claim.claimed_at_ms) <= ACCOUNT_USAGE_CLAIM_TTL.as_millis() as u64
    }) || inside_floor
        || oauth_read_is_fresh(entry, now_ms, cached_hint.as_ref())
    {
        return None;
    }
    let identity = cache_derived_claim_identity(entry, cached_hint.clone());
    entry.direct_query_claim = Some(DirectQueryClaim {
        nonce,
        claimed_at_ms: now_ms,
        hint: cached_hint,
        requested_scope: identity.scope,
        credentials_stamp: identity.credentials_stamp,
        preflight_account_key: identity.account_key,
    });
    cache.refreshed_at_ms = now_ms;
    write_credits_cache(&path, &cache);
    Some(nonce)
}

fn cache_derived_claim_identity(
    entry: &ProviderCreditsEntry,
    cached_hint: Option<AccountUsageIdentity>,
) -> AccountUsageIdentity {
    let prior = AccountUsageIdentity {
        scope: entry.scope.clone(),
        account_key: entry.account_key.clone(),
        credentials_stamp: entry.credentials_stamp,
    };
    let Some(mut hint) = cached_hint else {
        return prior;
    };
    if hint.scope == entry.scope {
        hint.account_key = hint.account_key.or(prior.account_key);
        hint.credentials_stamp = hint.credentials_stamp.or(prior.credentials_stamp);
    }
    hint
}

pub(super) fn account_usage_claim_matches(
    runtime: &RuntimePaths,
    key: &LoginKey,
    nonce: Uuid,
) -> bool {
    read_credits_cache(&runtime.shared_credits_path())
        .logins
        .get(key)
        .and_then(|entry| entry.direct_query_claim.as_ref())
        .is_some_and(|claim| claim.nonce == nonce)
}

/// Renew the matching live direct-query lease immediately before direct
/// provider work. A replaced claim or contended credits lock cannot be renewed.
pub(super) fn renew_provider_account_usage_claim(
    runtime: &RuntimePaths,
    key: &LoginKey,
    nonce: Uuid,
) -> bool {
    renew_provider_account_usage_claim_at(runtime, key, nonce, unix_now_ms())
}

fn renew_provider_account_usage_claim_at(
    runtime: &RuntimePaths,
    key: &LoginKey,
    nonce: Uuid,
    now_ms: u64,
) -> bool {
    let path = runtime.shared_credits_path();
    let Some(_guard) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_credits_lock())
            .ok()
            .flatten()
    else {
        return false;
    };
    let mut cache = read_credits_cache(&path);
    let Some(claim) = cache
        .logins
        .get_mut(key)
        .and_then(|entry| entry.direct_query_claim.as_mut())
        .filter(|claim| claim.nonce == nonce)
    else {
        return false;
    };
    claim.claimed_at_ms = now_ms;
    cache.refreshed_at_ms = now_ms;
    write_credits_cache(&path, &cache);
    true
}

pub(super) fn cancel_provider_account_usage_claim(
    runtime: &RuntimePaths,
    key: &LoginKey,
    nonce: Uuid,
) -> bool {
    let path = runtime.shared_credits_path();
    let Some(_guard) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_credits_lock())
            .ok()
            .flatten()
    else {
        return false;
    };
    let mut cache = read_credits_cache(&path);
    let Some(entry) = cache.logins.get_mut(key) else {
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

pub(super) fn complete_provider_account_usage(
    runtime: &RuntimePaths,
    key: &LoginKey,
    nonce: Uuid,
    probe: AccountUsageProbe,
) -> Option<AccountUsageCompletion> {
    let now_ms = unix_now_ms();
    let path = runtime.shared_credits_path();
    let _guard = crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_credits_lock())
        .ok()
        .flatten()?;
    let mut cache = read_credits_cache(&path);
    let prior = cache.logins.get(key).cloned()?;
    let (entry, completion) = prior.complete_account_usage(nonce, probe, now_ms)?;
    cache.refreshed_at_ms = now_ms;
    cache.logins.insert(key.to_owned(), entry);
    write_credits_cache(&path, &cache);
    Some(completion)
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

pub(super) fn invalidate_oauth_read(runtime: &RuntimePaths, key: &LoginKey) {
    let path = runtime.shared_credits_path();
    let Some(_guard) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.shared_credits_lock())
            .ok()
            .flatten()
    else {
        return;
    };
    let mut cache = read_credits_cache(&path);
    let Some(entry) = cache.logins.get_mut(key) else {
        return;
    };
    entry.invalidate_oauth_read();
    cache.refreshed_at_ms = unix_now_ms();
    write_credits_cache(&path, &cache);
}

fn oauth_read_is_fresh(
    entry: &ProviderCreditsEntry,
    now_ms: u64,
    cached_hint: Option<&AccountUsageIdentity>,
) -> bool {
    if entry.oauth_read_at_ms == 0 || entry.oauth_read_hint.as_ref() != cached_hint {
        return false;
    }
    let ttl = if entry.auth_settled {
        OAUTH_USAGE_SETTLED_TTL
    } else {
        OAUTH_USAGE_TTL
    };
    now_ms.saturating_sub(entry.oauth_read_at_ms) <= ttl.as_millis() as u64
}

/// The cached reset credits of each idle account, for auto-redeem's expiry
/// rescue. An idle account gets no panel, so its credits reach the harness
/// here, under the staleness line a panel's credits are held to.
pub(super) fn idle_reset_credits(
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
) -> BTreeMap<LoginKey, ResetCredits> {
    let cache = read_credits_cache(&runtime.shared_credits_path());
    idle_reset_credits_with(&cache, logins, unix_now_ms())
}

fn idle_reset_credits_with(
    cache: &CreditsCache,
    logins: &RoomLoginSet,
    now_ms: u64,
) -> BTreeMap<LoginKey, ResetCredits> {
    crate::harness::auto_redeem::idle_logins(logins)
        .into_iter()
        .filter_map(|login| {
            let key = login.key();
            let entry = cache.logins.get(&key)?;
            let credits = entry.reset_credits.clone()?;
            entry_is_displayable(entry, now_ms).then_some((key, credits))
        })
        .collect()
}

pub(in crate::sidebar) fn apply_credits_cache(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    accounts: &AccountsConfig,
    logins: &RoomLoginSet,
) {
    if snapshot.providers.is_empty() {
        return;
    }
    let cache = read_credits_cache(&runtime.shared_credits_path());
    apply_credits_cache_with(snapshot, &cache, accounts, logins, unix_now_ms());
}

fn apply_credits_cache_with(
    snapshot: &mut SidebarSnapshot,
    cache: &CreditsCache,
    accounts: &AccountsConfig,
    logins: &RoomLoginSet,
    now_ms: u64,
) {
    let in_use = logins.keys_in_use();
    for panel in &mut snapshot.providers {
        let entry = in_use
            .contains(&panel.login_key())
            .then(|| panel.login_key())
            .and_then(|key| cache.logins.get(&key))
            .filter(|entry| panel.metered && entry.scope == panel.account_scope);
        panel.entitlement = entry.map_or(Entitlement::Ok, |entry| entry.entitlement);
        if matches!(panel.entitlement, Entitlement::Lapsed { .. }) {
            panel.plan = None;
            panel.windows.clear();
            panel.extra_credits = None;
            panel.reset_credits = None;
            continue;
        }
        let ceiling = accounts.usage_limit(&panel.kind);
        if panel.metered {
            let displayable_entry = entry.filter(|entry| entry_is_displayable(entry, now_ms));
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
