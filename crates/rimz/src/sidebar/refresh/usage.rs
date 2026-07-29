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

use crate::agents::{AccountUsageIdentity, AccountUsageSnapshot, ProviderAccountScope};
use crate::ids::{AgentKind, WorkspaceId};
use crate::{RuntimePaths, SidebarSnapshot};

use super::accounts::cached_account_usage_hint;
use super::credits::{
    account_usage_claim_matches, cancel_provider_account_usage_claim, claim_provider_account_usage,
    complete_provider_account_usage, merge_provider_realtime_usage,
    renew_provider_account_usage_claim,
};
use super::trace;
use super::trace::{TraceEvent, duration_ms};
use super::{drop_kind_rate_limits, merge_account_rate_limits};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountUsageRefreshRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub claim_id: Uuid,
}

/// Claim and spawn each metered provider's direct account-usage refresh.
pub(crate) fn refresh_account_usage(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    refresh_account_usage_with(snapshot, runtime, spawn_usage_refresh);
}

fn refresh_account_usage_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    mut spawn: impl FnMut(&RuntimePaths, &str, Uuid) -> bool,
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
        let Some(adapter) = crate::agents::find_definition(kind) else {
            trace_claim(runtime, kind, "adapter_missing", started.elapsed());
            continue;
        };
        if !adapter.spec().capabilities.direct_account_usage {
            trace_claim(runtime, kind, "unsupported", started.elapsed());
            continue;
        }
        let cached_hint = cached_account_usage_hint(runtime, kind);
        let Some(claim_id) = claim_provider_account_usage(runtime, kind, cached_hint) else {
            trace_claim(runtime, kind, "not_due", started.elapsed());
            continue;
        };
        trace_claim(runtime, kind, "claimed", started.elapsed());
        let spawn_started = Instant::now();
        let spawned = spawn(runtime, kind, claim_id);
        trace::record(runtime, || TraceEvent::HelperSpawn {
            kind,
            outcome: if spawned { "spawned" } else { "failed" },
            elapsed_ms: duration_ms(spawn_started.elapsed()),
        });
        if !spawned {
            cancel_provider_account_usage_claim(runtime, kind, claim_id);
        }
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
/// provider call; late or superseded workers leave both caches untouched.
pub fn refresh_claimed_account_usage(runtime: &RuntimePaths, kind: &str, claim_id: Uuid) -> bool {
    let started = Instant::now();
    if crate::agents::credits::oauth_usage_offline() {
        cancel_provider_account_usage_claim(runtime, kind, claim_id);
        trace_usage_helper(runtime, kind, "offline", 0, 0, 0, started.elapsed());
        return false;
    }
    if !account_usage_claim_matches(runtime, kind, claim_id) {
        trace_usage_helper(runtime, kind, "superseded", 0, 0, 0, started.elapsed());
        return false;
    }
    let Some(adapter) = crate::agents::find_definition(kind) else {
        cancel_provider_account_usage_claim(runtime, kind, claim_id);
        trace_usage_helper(runtime, kind, "adapter_missing", 0, 0, 0, started.elapsed());
        return false;
    };
    let realtime_started = Instant::now();
    let realtime = adapter.probe_realtime_account_usage(runtime);
    let realtime_ms = duration_ms(realtime_started.elapsed());
    if !account_usage_claim_matches(runtime, kind, claim_id) {
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
            publish_account_usage_snapshot(runtime, kind, ProviderAccountScope::KindWide, usage);
        cache_publication_ms += duration_ms(publication_started.elapsed());
    }
    if !renew_provider_account_usage_claim(runtime, kind, claim_id) {
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
    let direct_probe = adapter.probe_account_usage();
    let direct_ms = duration_ms(direct_started.elapsed());
    let outcome = account_usage_outcome(&direct_probe);
    let publication_started = Instant::now();
    wrote |= complete_direct_account_usage(runtime, kind, claim_id, direct_probe);
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AccountUsageCompletionDecision {
    publish_realtime: bool,
    run_direct: bool,
}

fn account_usage_completion_decision(
    oauth_enabled: bool,
    realtime: Option<&AccountUsageSnapshot>,
) -> AccountUsageCompletionDecision {
    if !oauth_enabled {
        return AccountUsageCompletionDecision::default();
    }
    let Some(realtime) = realtime else {
        return AccountUsageCompletionDecision {
            run_direct: true,
            ..Default::default()
        };
    };
    AccountUsageCompletionDecision {
        publish_realtime: realtime.plan.is_some()
            || realtime.extra_credits.is_some()
            || realtime.reset_credits.is_some(),
        run_direct: realtime.plan.is_none()
            || realtime.extra_credits.is_none()
            || realtime.rate_limits.is_none(),
    }
}

/// Complete one synchronous provider refresh from optional realtime data and a
/// due direct probe. Codex uses this after its app-server read so publication,
/// fallback, and window precedence stay owned by the sidebar cache layer.
pub fn complete_realtime_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    oauth_enabled: bool,
    realtime: Option<AccountUsageSnapshot>,
) -> bool {
    complete_realtime_account_usage_with(
        runtime,
        kind,
        oauth_enabled,
        realtime,
        merge_account_usage_if_due,
    )
}

/// Force one bounded direct account-usage read before a reset-shaped ping.
/// Cache publication remains nonce-guarded by the normal claim path.
pub fn refresh_account_usage_now(runtime: &RuntimePaths, kind: &str) -> bool {
    if crate::agents::credits::oauth_usage_offline() {
        return false;
    }
    refresh_account_usage_now_with(runtime, kind, merge_account_usage_if_due)
}

/// Refresh one provider's account usage when its durable cadence says it is
/// due. The claim path single-flights concurrent callers.
pub fn refresh_account_usage_if_due(runtime: &RuntimePaths, kind: &str) -> bool {
    if crate::agents::credits::oauth_usage_offline() {
        return false;
    }
    merge_account_usage_if_due(runtime, kind)
}

fn refresh_account_usage_now_with(
    runtime: &RuntimePaths,
    kind: &str,
    refresh: impl FnOnce(&RuntimePaths, &str) -> bool,
) -> bool {
    super::credits::invalidate_oauth_read(runtime, kind);
    refresh(runtime, kind)
}

fn complete_realtime_account_usage_with(
    runtime: &RuntimePaths,
    kind: &str,
    oauth_enabled: bool,
    realtime: Option<AccountUsageSnapshot>,
    complete_direct: impl FnOnce(&RuntimePaths, &str) -> bool,
) -> bool {
    let decision = account_usage_completion_decision(oauth_enabled, realtime.as_ref());
    let mut wrote = false;
    if decision.publish_realtime
        && let Some(realtime) = realtime
    {
        wrote |=
            publish_account_usage_snapshot(runtime, kind, ProviderAccountScope::KindWide, realtime);
    }
    if decision.run_direct {
        wrote |= complete_direct(runtime, kind);
    }
    wrote
}

/// Claim and execute a direct read in-process. Codex synchronous refresh paths
/// use this instead of maintaining a separate cadence.
fn merge_account_usage_if_due(runtime: &RuntimePaths, kind: &str) -> bool {
    let Some(adapter) = crate::agents::find_definition(kind) else {
        return false;
    };
    if !adapter.spec().capabilities.direct_account_usage {
        return false;
    }
    let cached_hint = cached_account_usage_hint(runtime, kind);
    let Some(claim_id) = claim_provider_account_usage(runtime, kind, cached_hint) else {
        return false;
    };
    complete_direct_account_usage(runtime, kind, claim_id, adapter.probe_account_usage())
}

fn complete_direct_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    claim_id: Uuid,
    probe: crate::agents::AccountUsageProbe,
) -> bool {
    let Some(completion) = complete_provider_account_usage(runtime, kind, claim_id, probe) else {
        return false;
    };
    if completion.account_changed {
        tracing::info!(
            target: crate::observability::BREADCRUMB_TARGET,
            kind,
            "provider account changed; dropping cached windows",
        );
        drop_kind_rate_limits(runtime, kind);
    }
    if let Some(snapshot) = completion.snapshot {
        publish_account_usage_windows(runtime, kind, completion.identity, snapshot.rate_limits);
    }
    true
}

/// Publish one normalized realtime snapshot. Credits and optional windows keep
/// their owning locks and no provider call runs while either lock is held.
fn publish_account_usage_snapshot(
    runtime: &RuntimePaths,
    kind: &str,
    scope: ProviderAccountScope,
    mut snapshot: AccountUsageSnapshot,
) -> bool {
    let windows = snapshot.rate_limits.take();
    let has_credits = snapshot.plan.is_some()
        || snapshot.extra_credits.is_some()
        || snapshot.reset_credits.is_some();
    if has_credits {
        merge_provider_realtime_usage(runtime, kind, scope.clone(), snapshot);
    }
    let has_windows = publish_account_usage_windows(
        runtime,
        kind,
        AccountUsageIdentity {
            scope,
            ..AccountUsageIdentity::default()
        },
        windows,
    );
    has_credits || has_windows
}

fn publish_account_usage_windows(
    runtime: &RuntimePaths,
    kind: &str,
    identity: AccountUsageIdentity,
    windows: Option<crate::agents::AgentRateLimits>,
) -> bool {
    let Some(windows) = windows else {
        return false;
    };
    merge_account_rate_limits(runtime, kind, identity, windows);
    true
}

fn spawn_usage_refresh(runtime: &RuntimePaths, kind: &str, claim_id: Uuid) -> bool {
    let exe = crate::proc::rimz_exe();
    let mut cmd = crate::child_process::detached_rimz_command(exe, runtime);
    let request = AccountUsageRefreshRequest {
        workspace_id: runtime.workspace_id.clone(),
        kind: AgentKind::new_unchecked(kind),
        claim_id,
    };
    cmd.args(crate::child_process::agent_helper_argv(
        "refresh-usage",
        &request,
    ));
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind,
        "sidebar: spawning account usage refresh",
    );
    match crate::child_process::spawn_detached_reaped(&mut cmd, "agents-refresh-usage") {
        Ok(_) => true,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                kind,
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
