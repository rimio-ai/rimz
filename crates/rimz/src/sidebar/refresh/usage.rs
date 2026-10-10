//! Uniform provider account-usage refresh.
//!
//! Sidebar producers claim due direct reads in `credits.json` before spawning a
//! helper. Provider calls run outside cache locks; a matching nonce alone may
//! publish the result. Realtime and direct readings share one snapshot shape
//! and one cache-publication path, with window precedence resolved during
//! per-frame fusion.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::RuntimePaths;
use crate::agents::{
    AccountUsageIdentity, AccountUsageSnapshot, Entitlement, ProviderLogin, RoomLoginSet,
};
use crate::ids::{LoginKey, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;

use super::accounts::cached_account_usage_hint;
use super::credits::{
    account_usage_claim_matches, cancel_provider_account_usage_claim, claim_idle_account_usage,
    claim_provider_account_usage, complete_provider_account_usage, merge_provider_realtime_usage,
    read_credits_cache, renew_provider_account_usage_claim, write_credits_cache,
};
use super::rate_limits::{drop_login_rate_limits, merge_account_rate_limits};
use super::trace;
use super::trace::{TraceEvent, duration_ms};

/// Record subscription-access rejection for a login and make its next direct read due.
pub fn mark_entitlement_lapsed(runtime: &RuntimePaths, key: &LoginKey, at_ms: u64) {
    let _guard = match crate::disk::lock::WorkspaceLock::acquire(&runtime.shared_credits_lock()) {
        Ok(guard) => guard,
        Err(err) => {
            tracing::warn!(login = %key, error = %err, "entitlement marker could not lock credits cache");
            return;
        }
    };
    let path = runtime.shared_credits_path();
    let mut cache = read_credits_cache(&path);
    let entry = cache.logins.entry(key.clone()).or_default();
    if entry.entitlement == Entitlement::Ok {
        entry.entitlement = Entitlement::Lapsed { since_ms: at_ms };
    }
    entry.invalidate_oauth_read();
    cache.refreshed_at_ms = crate::utils::time::unix_now_ms();
    write_credits_cache(&path, &cache);
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountUsageRefreshRequest {
    pub workspace_id: WorkspaceId,
    pub login: LoginKey,
    pub claim_id: Uuid,
}

/// Claim and spawn each metered provider's direct account-usage refresh, then
/// each idle account's.
pub(super) fn refresh_account_usage(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
) {
    refresh_account_usage_with(snapshot, runtime, logins, spawn_usage_refresh);
}

fn refresh_account_usage_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &RoomLoginSet,
    mut spawn: impl FnMut(&RuntimePaths, &LoginKey, Uuid) -> bool,
) {
    if crate::agents::credits::oauth_usage_offline() {
        return;
    }
    for panel in &snapshot.providers {
        if !panel.metered {
            continue;
        }
        let started = Instant::now();
        let kind = panel.kind.as_str();
        let Some(login) = logins
            .in_use(kind)
            .into_iter()
            .find(|login| login.key() == panel.login_key())
        else {
            trace_claim(runtime, kind, "login_unresolved", started.elapsed());
            continue;
        };
        claim_and_spawn(
            runtime,
            &login.key(),
            started,
            claim_provider_account_usage,
            &mut spawn,
        );
    }
    // An idle account has no panel: auto-redeem's expiry rescue is its one
    // reader, so it is claimed on the slow idle floor.
    for login in crate::harness::auto_redeem::idle_logins(logins) {
        claim_and_spawn(
            runtime,
            &login.key(),
            Instant::now(),
            claim_idle_account_usage,
            &mut spawn,
        );
    }
}

fn claim_and_spawn(
    runtime: &RuntimePaths,
    key: &LoginKey,
    started: Instant,
    claim: fn(&RuntimePaths, &LoginKey, Option<AccountUsageIdentity>) -> Option<Uuid>,
    spawn: &mut impl FnMut(&RuntimePaths, &LoginKey, Uuid) -> bool,
) {
    let kind = key.kind.as_str();
    let Some(adapter) = crate::agents::find_definition(kind) else {
        trace_claim(runtime, kind, "adapter_missing", started.elapsed());
        return;
    };
    if !adapter.spec().capabilities.direct_account_usage {
        trace_claim(runtime, kind, "unsupported", started.elapsed());
        return;
    }
    let cached_hint = cached_account_usage_hint(runtime, key);
    let Some(claim_id) = claim(runtime, key, cached_hint) else {
        trace_claim(runtime, kind, "not_due", started.elapsed());
        return;
    };
    trace_claim(runtime, kind, "claimed", started.elapsed());
    let spawn_started = Instant::now();
    let spawned = spawn(runtime, key, claim_id);
    trace::record(runtime, || TraceEvent::HelperSpawn {
        kind,
        outcome: if spawned { "spawned" } else { "failed" },
        elapsed_ms: duration_ms(spawn_started.elapsed()),
    });
    if !spawned {
        cancel_provider_account_usage_claim(runtime, key, claim_id);
    }
}

fn trace_claim(runtime: &RuntimePaths, kind: &str, outcome: &str, elapsed: Duration) {
    let elapsed_ms = duration_ms(elapsed);
    trace::record(runtime, || TraceEvent::Claim {
        kind,
        outcome,
        elapsed_ms,
    });
}

/// Run one producer-created claim. The helper validates the nonce before any
/// provider call; late or superseded workers leave both caches untouched. A
/// login neither in use nor an idle account cancels its claim.
pub fn refresh_claimed_account_usage(
    runtime: &RuntimePaths,
    key: &LoginKey,
    claim_id: Uuid,
) -> bool {
    refresh_claimed_account_usage_with(
        runtime,
        key,
        claim_id,
        &crate::store::room_logins_in_use(runtime),
        |adapter, runtime, env| adapter.probe_realtime_account_usage(runtime, env),
        |adapter, env| adapter.probe_account_usage(env),
    )
}

fn refresh_claimed_account_usage_with(
    runtime: &RuntimePaths,
    key: &LoginKey,
    claim_id: Uuid,
    set: &RoomLoginSet,
    probe_realtime: impl FnOnce(
        &crate::agents::AgentDefinition,
        &RuntimePaths,
        &std::collections::BTreeMap<String, String>,
    ) -> Option<AccountUsageSnapshot>,
    probe: impl FnOnce(
        &crate::agents::AgentDefinition,
        &std::collections::BTreeMap<String, String>,
    ) -> crate::agents::AccountUsageProbe,
) -> bool {
    let started = Instant::now();
    let kind = key.kind.as_str();
    let is_key = |login: &ProviderLogin| login.key() == *key;
    let in_use = set.in_use(kind).into_iter().find(is_key);
    let idle = in_use.is_none();
    let Some(login) = in_use.or_else(|| {
        crate::harness::auto_redeem::idle_logins(set)
            .into_iter()
            .find(is_key)
    }) else {
        cancel_provider_account_usage_claim(runtime, key, claim_id);
        trace_usage_helper(
            runtime,
            kind,
            "login_undeclared",
            0,
            0,
            0,
            started.elapsed(),
        );
        return false;
    };
    let login_env = set.env(&login);
    if crate::agents::credits::oauth_usage_offline() {
        cancel_provider_account_usage_claim(runtime, key, claim_id);
        trace_usage_helper(runtime, kind, "offline", 0, 0, 0, started.elapsed());
        return false;
    }
    if !account_usage_claim_matches(runtime, key, claim_id) {
        trace_usage_helper(runtime, kind, "superseded", 0, 0, 0, started.elapsed());
        return false;
    }
    let Some(adapter) = crate::agents::find_definition(kind) else {
        cancel_provider_account_usage_claim(runtime, key, claim_id);
        trace_usage_helper(runtime, kind, "adapter_missing", 0, 0, 0, started.elapsed());
        return false;
    };
    let realtime_started = Instant::now();
    // The realtime leg asks this room's app server, which speaks for the
    // room's own login; an idle account is read by the direct probe alone.
    let realtime = if idle {
        None
    } else {
        probe_realtime(adapter, runtime, &login_env)
    };
    let realtime_ms = duration_ms(realtime_started.elapsed());
    if !account_usage_claim_matches(runtime, key, claim_id) {
        trace_usage_helper(
            runtime,
            kind,
            "superseded",
            realtime_ms,
            0,
            0,
            started.elapsed(),
        );
        return false;
    }
    let mut wrote = false;
    let mut cache_publication_ms = 0;
    if let Some(usage) = realtime {
        let publication_started = Instant::now();
        wrote |=
            publish_account_usage_snapshot(runtime, key, AccountUsageIdentity::default(), usage);
        cache_publication_ms += duration_ms(publication_started.elapsed());
    }
    if !renew_provider_account_usage_claim(runtime, key, claim_id) {
        trace_usage_helper(
            runtime,
            kind,
            "renewal_failed",
            realtime_ms,
            0,
            cache_publication_ms,
            started.elapsed(),
        );
        return wrote;
    }
    let direct_started = Instant::now();
    let direct_probe = probe(adapter, &login_env);
    let direct_ms = duration_ms(direct_started.elapsed());
    let outcome = account_usage_outcome(&direct_probe);
    let publication_started = Instant::now();
    wrote |= complete_direct_account_usage(runtime, key, claim_id, direct_probe);
    cache_publication_ms += duration_ms(publication_started.elapsed());
    trace_usage_helper(
        runtime,
        kind,
        outcome,
        realtime_ms,
        direct_ms,
        cache_publication_ms,
        started.elapsed(),
    );
    wrote
}

fn account_usage_outcome(probe: &crate::agents::AccountUsageProbe) -> &'static str {
    match probe {
        crate::agents::AccountUsageProbe::Found { .. } => "success",
        crate::agents::AccountUsageProbe::NoCredentials(_) => "no_credentials",
        crate::agents::AccountUsageProbe::NotEntitled(_) => "not_entitled",
        crate::agents::AccountUsageProbe::Failed(_) => "failed",
        crate::agents::AccountUsageProbe::Unsupported => "unsupported",
    }
}

fn trace_usage_helper(
    runtime: &RuntimePaths,
    kind: &str,
    outcome: &str,
    realtime_ms: u64,
    direct_ms: u64,
    cache_publication_ms: u64,
    total: Duration,
) {
    let total_ms = duration_ms(total);
    trace::record(runtime, || TraceEvent::UsageHelper {
        kind,
        outcome,
        realtime_ms,
        direct_ms,
        cache_publication_ms,
        total_ms,
    });
}

/// Complete one synchronous provider refresh from realtime data and a
/// due direct probe. Codex uses this after its app-server read so publication,
/// fallback, and window precedence stay owned by the sidebar cache layer.
pub fn complete_realtime_account_usage(
    runtime: &RuntimePaths,
    login: &ProviderLogin,
    realtime: AccountUsageSnapshot,
) -> bool {
    complete_realtime_account_usage_with(runtime, &login.key(), realtime, |runtime, _| {
        merge_account_usage_if_due(runtime, login)
    })
}

/// Refresh one provider's account usage for `rimz providers` and `rimz accounts list`, forcing a read
/// when requested or otherwise following its durable cadence.
/// Cache publication remains nonce-guarded by the normal claim path.
pub fn refresh_provider_usage(runtime: &RuntimePaths, login: &ProviderLogin, force: bool) -> bool {
    if crate::agents::credits::oauth_usage_offline() {
        return false;
    }
    refresh_provider_usage_with(runtime, login, force, merge_account_usage_if_due)
}

fn refresh_provider_usage_with(
    runtime: &RuntimePaths,
    login: &ProviderLogin,
    force: bool,
    refresh: impl FnOnce(&RuntimePaths, &ProviderLogin) -> bool,
) -> bool {
    if force {
        super::credits::invalidate_oauth_read(runtime, &login.key());
    }
    refresh(runtime, login)
}

fn complete_realtime_account_usage_with(
    runtime: &RuntimePaths,
    key: &LoginKey,
    realtime: AccountUsageSnapshot,
    complete_direct: impl FnOnce(&RuntimePaths, &LoginKey) -> bool,
) -> bool {
    if crate::agents::credits::oauth_usage_offline() {
        return false;
    }
    let publish = realtime.plan.is_some()
        || realtime.extra_credits.is_some()
        || realtime.reset_credits.is_some();
    let run_direct = realtime.plan.is_none()
        || realtime.extra_credits.is_none()
        || realtime.rate_limits.is_none();
    let mut wrote = false;
    if publish {
        wrote |=
            publish_account_usage_snapshot(runtime, key, AccountUsageIdentity::default(), realtime);
    }
    if run_direct {
        wrote |= complete_direct(runtime, key);
    }
    wrote
}

/// Claim and execute a direct read in-process. Codex synchronous refresh paths
/// use this instead of maintaining a separate cadence.
fn merge_account_usage_if_due(runtime: &RuntimePaths, login: &ProviderLogin) -> bool {
    let login_env = login.env(&crate::agents::ambient_env());
    let Some(adapter) = crate::agents::find_definition(login.kind().as_str()) else {
        return false;
    };
    if !adapter.spec().capabilities.direct_account_usage {
        return false;
    }
    let key = login.key();
    let cached_hint = cached_account_usage_hint(runtime, &key);
    let Some(claim_id) = claim_provider_account_usage(runtime, &key, cached_hint) else {
        return false;
    };
    complete_direct_account_usage(
        runtime,
        &key,
        claim_id,
        adapter.probe_account_usage(&login_env),
    )
}

fn complete_direct_account_usage(
    runtime: &RuntimePaths,
    key: &LoginKey,
    claim_id: Uuid,
    probe: crate::agents::AccountUsageProbe,
) -> bool {
    let Some(completion) = complete_provider_account_usage(runtime, key, claim_id, probe) else {
        return false;
    };
    if completion.account_changed {
        tracing::info!(
            target: crate::observability::BREADCRUMB_TARGET,
            kind = key.kind.as_str(),
            "provider account changed; dropping cached windows",
        );
        drop_login_rate_limits(runtime, key);
    }
    if let Some(snapshot) = completion.snapshot {
        publish_account_usage_windows(runtime, key, completion.identity, snapshot.rate_limits);
    }
    true
}

/// Publish one normalized realtime snapshot. Credits and optional windows keep
/// their owning locks and no provider call runs while either lock is held.
/// This is the one entry through which any caller publishes a normalized
/// account-usage snapshot, harness-originated redemptions included.
pub fn publish_account_usage_snapshot(
    runtime: &RuntimePaths,
    key: &LoginKey,
    identity: AccountUsageIdentity,
    mut snapshot: AccountUsageSnapshot,
) -> bool {
    let windows = snapshot.rate_limits.take();
    let has_credits = snapshot.plan.is_some()
        || snapshot.extra_credits.is_some()
        || snapshot.reset_credits.is_some();
    if has_credits {
        merge_provider_realtime_usage(runtime, key, identity.scope.clone(), snapshot);
    }
    let has_windows = publish_account_usage_windows(runtime, key, identity, windows);
    has_credits || has_windows
}

fn publish_account_usage_windows(
    runtime: &RuntimePaths,
    key: &LoginKey,
    identity: AccountUsageIdentity,
    windows: Option<crate::agents::AgentRateLimits>,
) -> bool {
    let Some(windows) = windows else {
        return false;
    };
    merge_account_rate_limits(runtime, key, identity, windows);
    true
}

fn spawn_usage_refresh(runtime: &RuntimePaths, key: &LoginKey, claim_id: Uuid) -> bool {
    let exe = crate::proc::rimz_exe();
    let mut cmd = crate::child_process::detached_rimz_command(exe, runtime);
    let request = AccountUsageRefreshRequest {
        workspace_id: runtime.workspace_id.clone(),
        login: key.clone(),
        claim_id,
    };
    cmd.args(crate::child_process::agent_helper_argv(
        "refresh-usage",
        &request,
    ));
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = key.kind.as_str(),
        "sidebar: spawning account usage refresh",
    );
    match crate::child_process::spawn_detached_reaped(&mut cmd, "agents-refresh-usage") {
        Ok(_) => true,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                kind = key.kind.as_str(),
                tags.operation = "agents.usage_refresh.spawn",
                error = &err as &dyn std::error::Error,
                "sidebar: failed to spawn account usage refresh",
            );
            false
        }
    }
}

#[cfg(test)]
mod tests;
