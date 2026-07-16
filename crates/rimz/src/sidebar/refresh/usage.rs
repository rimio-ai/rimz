//! Uniform provider account-usage refresh.
//!
//! Sidebar producers claim due direct reads in `credits.json` before spawning a
//! helper. Provider calls run outside cache locks; a matching nonce alone may
//! publish the result. Realtime and direct readings share one snapshot shape
//! and one cache-publication path.

use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::agents::{AccountUsageIdentity, AccountUsageSnapshot, ProviderAccountScope};
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

/// Claim and spawn each metered provider's direct account-usage refresh.
pub(crate) fn refresh_account_usage(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    refresh_account_usage_with(snapshot, runtime, spawn_usage_refresh);
}

fn refresh_account_usage_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    mut spawn: impl FnMut(&RuntimePaths, &str, bool, Uuid) -> bool,
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
        let Some(adapter) = crate::agents::find_adapter(kind) else {
            trace_claim(runtime, kind, "adapter_missing", started.elapsed());
            continue;
        };
        let Some(identity) = scheduling_identity(runtime, kind, adapter) else {
            trace_claim(runtime, kind, "unsupported", started.elapsed());
            continue;
        };
        let Some(claim_id) = claim_provider_account_usage(runtime, kind, identity) else {
            trace_claim(runtime, kind, "not_due", started.elapsed());
            continue;
        };
        trace_claim(runtime, kind, "claimed", started.elapsed());
        let spawn_started = Instant::now();
        let spawned = spawn(runtime, kind, merge_windows_hint(snapshot, kind), claim_id);
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

fn scheduling_identity(
    runtime: &RuntimePaths,
    kind: &str,
    adapter: &dyn crate::agents::AgentAdapter,
) -> Option<AccountUsageIdentity> {
    let mut identity = adapter.account_usage_identity()?;
    let cached_hint = cached_account_usage_hint(runtime, kind);
    if let Some((scope, Some(credentials_stamp))) = cached_hint.as_ref() {
        identity.scope = scope.clone();
        identity.credentials_stamp = Some(*credentials_stamp);
    } else if let Some((scope, stamp)) = cached_hint {
        identity.scope = scope;
        identity.credentials_stamp = identity.credentials_stamp.or(stamp);
    }
    if identity.account_key.is_none() {
        identity.account_key = super::credits::read_credits_cache(&runtime.shared_credits_path())
            .entries
            .get(kind)
            .filter(|entry| entry.scope == identity.scope)
            .and_then(|entry| entry.account_key.clone());
    }
    Some(identity)
}

/// Run one producer-created claim. The helper validates the nonce before any
/// provider call; late or superseded workers leave both caches untouched.
pub fn refresh_claimed_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    claim_id: Uuid,
    merge_windows: bool,
) -> bool {
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
    let Some(adapter) = crate::agents::find_adapter(kind) else {
        cancel_provider_account_usage_claim(runtime, kind, claim_id);
        trace_usage_helper(runtime, kind, "adapter_missing", 0, 0, 0, started.elapsed());
        return false;
    };
    let realtime_started = Instant::now();
    let realtime = adapter.probe_realtime_account_usage(runtime);
    let realtime_ms = duration_ms(realtime_started.elapsed());
    let publish_direct_windows = direct_windows_should_publish(
        merge_windows,
        realtime.as_ref(),
        adapter
            .descriptor()
            .capabilities
            .realtime_usage
            .windows_defer_to_fresh_realtime,
    );
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
        wrote |= publish_account_usage_snapshot(
            runtime,
            kind,
            ProviderAccountScope::KindWide,
            usage,
            true,
        );
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
    wrote |= complete_direct_account_usage(
        runtime,
        kind,
        claim_id,
        direct_probe,
        publish_direct_windows,
    );
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

fn direct_windows_should_publish(
    requested: bool,
    realtime: Option<&AccountUsageSnapshot>,
    defer_to_fresh_realtime: bool,
) -> bool {
    requested
        || realtime.is_some_and(|usage| usage.rate_limits.is_none())
        || (realtime.is_none() && !defer_to_fresh_realtime)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AccountUsageCompletionDecision {
    publish_realtime: bool,
    run_direct: bool,
    merge_direct_windows: bool,
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
            merge_direct_windows: true,
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
        merge_direct_windows: realtime.rate_limits.is_none(),
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
    let decision = account_usage_completion_decision(oauth_enabled, realtime.as_ref());
    let mut wrote = false;
    if decision.publish_realtime {
        wrote |= publish_account_usage_snapshot(
            runtime,
            kind,
            ProviderAccountScope::KindWide,
            realtime
                .as_ref()
                .expect("publish decision requires realtime usage")
                .clone(),
            true,
        );
    }
    if decision.run_direct {
        wrote |= merge_account_usage_if_due(runtime, kind, decision.merge_direct_windows);
    }
    wrote
}

/// Claim and execute a direct read in-process. Codex synchronous refresh paths
/// use this instead of maintaining a separate cadence.
pub fn merge_account_usage_if_due(runtime: &RuntimePaths, kind: &str, merge_windows: bool) -> bool {
    let Some(adapter) = crate::agents::find_adapter(kind) else {
        return false;
    };
    let Some(identity) = scheduling_identity(runtime, kind, adapter) else {
        return false;
    };
    let Some(claim_id) = claim_provider_account_usage(runtime, kind, identity) else {
        return false;
    };
    complete_direct_account_usage(
        runtime,
        kind,
        claim_id,
        adapter.probe_account_usage(),
        merge_windows,
    )
}

fn complete_direct_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    claim_id: Uuid,
    probe: crate::agents::AccountUsageProbe,
    merge_windows: bool,
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
        publish_account_usage_windows(
            runtime,
            kind,
            completion.identity.scope,
            snapshot.rate_limits,
            merge_windows,
        );
    }
    true
}

/// Publish one normalized realtime snapshot. Credits and optional windows keep
/// their owning locks and no provider call runs while either lock is held.
pub fn publish_account_usage_snapshot(
    runtime: &RuntimePaths,
    kind: &str,
    scope: ProviderAccountScope,
    mut snapshot: AccountUsageSnapshot,
    publish_windows: bool,
) -> bool {
    let windows = snapshot.rate_limits.take();
    let has_credits = snapshot.plan.is_some()
        || snapshot.extra_credits.is_some()
        || snapshot.reset_credits.is_some();
    if has_credits {
        merge_provider_realtime_usage(runtime, kind, scope.clone(), snapshot);
    }
    let has_windows = publish_account_usage_windows(runtime, kind, scope, windows, publish_windows);
    has_credits || has_windows
}

fn publish_account_usage_windows(
    runtime: &RuntimePaths,
    kind: &str,
    scope: ProviderAccountScope,
    windows: Option<crate::agents::AgentRateLimits>,
    publish: bool,
) -> bool {
    let Some(windows) = windows.filter(|_| publish) else {
        return false;
    };
    merge_account_rate_limits(runtime, kind, scope, windows);
    true
}

fn merge_windows_hint(snapshot: &SidebarSnapshot, kind: &str) -> bool {
    !crate::agents::descriptor_by_kind(kind).is_some_and(|descriptor| {
        descriptor
            .capabilities
            .realtime_usage
            .windows_defer_to_fresh_realtime
            && has_fresh_realtime_windows(snapshot, kind)
    })
}

fn has_fresh_realtime_windows(snapshot: &SidebarSnapshot, kind: &str) -> bool {
    let now = snapshot.now;
    snapshot.agents.iter().any(|agent| {
        if agent.kind.as_str() != kind || agent.parent_agent_id.is_some() {
            return false;
        }
        let Some(context) = agent.context.as_ref() else {
            return false;
        };
        let Some(limits) = context.rate_limits.as_ref() else {
            return false;
        };
        !limits.windows.is_empty()
            && !limits.content_stale_at(now)
            && now.duration_since(context.observed_at).as_secs()
                <= crate::sidebar::timing::CREDITS_TTL.as_secs() as i64
    })
}

fn spawn_usage_refresh(
    runtime: &RuntimePaths,
    kind: &str,
    merge_windows: bool,
    claim_id: Uuid,
) -> bool {
    let exe = crate::proc::rimz_exe();
    let mut cmd = crate::child_process::detached_rimz_command(exe, runtime);
    cmd.args([
        "agents",
        "refresh-usage",
        "--kind",
        kind,
        "--workspace-id",
        runtime.workspace_id.as_str(),
        "--claim-id",
        &claim_id.to_string(),
    ]);
    if merge_windows {
        cmd.arg("--merge-windows");
    }
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind,
        merge_windows,
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
mod tests {
    use std::collections::BTreeMap;

    use jiff::Timestamp;

    use crate::agents::account::read_rate_limits_cache;
    use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow};
    use crate::ids::WorkspaceId;
    use crate::sidebar::refresh::accounts::{AccountsCache, ProviderRecord};
    use crate::sidebar::refresh::credits::{CreditsCache, ProviderCreditsEntry};
    use crate::sidebar::test_support::{provider_panel, snapshot_with_panels};

    use super::*;

    fn complete_realtime() -> AccountUsageSnapshot {
        AccountUsageSnapshot {
            plan: Some("pro".to_owned()),
            extra_credits: Some(crate::agents::ExtraCredits::Disabled),
            rate_limits: Some(AgentRateLimits::default()),
            reset_credits: None,
        }
    }

    #[test]
    fn account_usage_completion_decision_matrix() {
        let complete = complete_realtime();
        assert_eq!(
            account_usage_completion_decision(false, Some(&complete)),
            AccountUsageCompletionDecision::default()
        );
        assert_eq!(
            account_usage_completion_decision(true, None),
            AccountUsageCompletionDecision {
                publish_realtime: false,
                run_direct: true,
                merge_direct_windows: true,
            }
        );
        assert_eq!(
            account_usage_completion_decision(true, Some(&complete)),
            AccountUsageCompletionDecision {
                publish_realtime: true,
                run_direct: false,
                merge_direct_windows: false,
            }
        );

        let mut missing_plan = complete.clone();
        missing_plan.plan = None;
        assert_eq!(
            account_usage_completion_decision(true, Some(&missing_plan)),
            AccountUsageCompletionDecision {
                publish_realtime: true,
                run_direct: true,
                merge_direct_windows: false,
            }
        );

        let mut missing_extra = complete.clone();
        missing_extra.extra_credits = None;
        assert_eq!(
            account_usage_completion_decision(true, Some(&missing_extra)),
            AccountUsageCompletionDecision {
                publish_realtime: true,
                run_direct: true,
                merge_direct_windows: false,
            }
        );

        let mut missing_windows = complete.clone();
        missing_windows.rate_limits = None;
        assert_eq!(
            account_usage_completion_decision(true, Some(&missing_windows)),
            AccountUsageCompletionDecision {
                publish_realtime: true,
                run_direct: true,
                merge_direct_windows: true,
            }
        );

        let reset_only = AccountUsageSnapshot {
            reset_credits: Some(crate::agents::ResetCredits {
                count: 1,
                soonest_expiry: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            account_usage_completion_decision(true, Some(&reset_only)),
            AccountUsageCompletionDecision {
                publish_realtime: true,
                run_direct: true,
                merge_direct_windows: true,
            }
        );
    }

    #[test]
    fn fresh_realtime_claude_windows_defer_direct_window_publication() {
        let mut snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/usage-refresh")),
            vec![crate::testkit::agent_state(
                "claude",
                "one",
                Timestamp::now(),
            )],
            Timestamp::now(),
        );
        snapshot.agents[0].context = Some(crate::store::agent_context::empty_context(
            "claude",
            snapshot.now,
        ));
        snapshot.agents[0].context.as_mut().unwrap().rate_limits = Some(AgentRateLimits {
            windows: vec![RateLimitWindow {
                duration_mins: Some(300),
                used_percentage: Some(12),
                resets_at: snapshot
                    .now
                    .checked_add(jiff::SignedDuration::from_hours(1))
                    .ok(),
                ..Default::default()
            }],
        });
        assert!(!merge_windows_hint(&snapshot, "claude"));
        assert!(merge_windows_hint(&snapshot, "codex"));
    }

    #[test]
    fn realtime_window_fallback_preserves_explicit_statusline_defer() {
        assert!(!direct_windows_should_publish(false, None, true));
        assert!(direct_windows_should_publish(false, None, false));
        assert!(direct_windows_should_publish(
            false,
            Some(&AccountUsageSnapshot::default()),
            true,
        ));
        assert!(!direct_windows_should_publish(
            false,
            Some(&AccountUsageSnapshot {
                rate_limits: Some(AgentRateLimits::default()),
                ..Default::default()
            }),
            true,
        ));
    }

    fn owned_usage_runtime(owner: &str) -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        super::super::credits::write_credits_cache(
            &runtime.shared_credits_path(),
            &CreditsCache {
                entries: BTreeMap::from([(
                    "antigravity".to_owned(),
                    ProviderCreditsEntry {
                        account_key: Some(owner.to_owned()),
                        plan: Some("old plan".to_owned()),
                        ok: true,
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        super::super::merge_account_rate_limits(
            &runtime,
            "antigravity",
            ProviderAccountScope::KindWide,
            AgentRateLimits {
                windows: vec![RateLimitWindow {
                    duration_mins: Some(300),
                    used_percentage: Some(88),
                    source: crate::agents::context::WindowSource::Authoritative,
                    ..Default::default()
                }],
            },
        );
        (dir, runtime)
    }

    fn usage_identity(owner: Option<&str>) -> AccountUsageIdentity {
        AccountUsageIdentity {
            account_key: owner.map(ToOwned::to_owned),
            ..Default::default()
        }
    }

    fn claim(runtime: &RuntimePaths) -> Uuid {
        claim_provider_account_usage(runtime, "antigravity", Default::default()).unwrap()
    }

    fn windows(runtime: &RuntimePaths) -> Vec<RateLimitWindow> {
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .get("antigravity")
            .map(|entry| entry.limits.windows.clone())
            .unwrap_or_default()
    }

    #[test]
    fn direct_account_usage_completion_replaces_or_drops_windows_only_for_a_known_new_owner() {
        let (_dir, runtime) = owned_usage_runtime("owner-a");
        assert!(complete_direct_account_usage(
            &runtime,
            "antigravity",
            claim(&runtime),
            crate::agents::AccountUsageProbe::Found {
                identity: usage_identity(Some("owner-b")),
                snapshot: AccountUsageSnapshot {
                    plan: Some("new plan".to_owned()),
                    rate_limits: Some(AgentRateLimits {
                        windows: vec![RateLimitWindow {
                            duration_mins: Some(300),
                            used_percentage: Some(12),
                            source: crate::agents::context::WindowSource::Authoritative,
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                },
            },
            true,
        ));
        assert_eq!(windows(&runtime)[0].used_percentage, Some(12));
        assert_eq!(
            super::super::credits::read_credits_cache(&runtime.shared_credits_path()).entries
                ["antigravity"]
                .account_key
                .as_deref(),
            Some("owner-b")
        );

        let (_dir, runtime) = owned_usage_runtime("owner-a");
        assert!(complete_direct_account_usage(
            &runtime,
            "antigravity",
            claim(&runtime),
            crate::agents::AccountUsageProbe::Failed(usage_identity(Some("owner-b"))),
            true,
        ));
        assert!(windows(&runtime).is_empty());

        for failed_identity in [usage_identity(None), usage_identity(Some("owner-a"))] {
            let (_dir, runtime) = owned_usage_runtime("owner-a");
            assert!(complete_direct_account_usage(
                &runtime,
                "antigravity",
                claim(&runtime),
                crate::agents::AccountUsageProbe::Failed(failed_identity),
                true,
            ));
            assert_eq!(windows(&runtime)[0].used_percentage, Some(88));
        }
    }

    #[test]
    fn unknown_owner_source_reuses_same_scope_owner_until_ttl() {
        let (_dir, runtime) = owned_usage_runtime("owner-a");
        assert!(complete_direct_account_usage(
            &runtime,
            "antigravity",
            claim(&runtime),
            crate::agents::AccountUsageProbe::Found {
                identity: usage_identity(Some("owner-a")),
                snapshot: AccountUsageSnapshot::default(),
            },
            true,
        ));

        let adapter = crate::agents::find_adapter("antigravity").unwrap();
        let identity = scheduling_identity(&runtime, "antigravity", adapter).unwrap();
        assert_eq!(identity.account_key.as_deref(), Some("owner-a"));
        assert_eq!(
            claim_provider_account_usage(&runtime, "antigravity", identity),
            None
        );
    }

    #[test]
    fn metered_adapter_without_usage_source_creates_no_claim_or_helper() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let snapshot = snapshot_with_panels(workspace, vec![provider_panel("cursor", Vec::new())]);
        let mut spawn_attempts = 0;
        refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
            spawn_attempts += 1;
            true
        });
        assert_eq!(spawn_attempts, 0);
        assert!(
            !super::super::credits::read_credits_cache(&runtime.shared_credits_path())
                .entries
                .contains_key("cursor")
        );
    }

    #[test]
    fn failed_spawn_cancels_claim_for_immediate_retry() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        super::super::accounts::write_accounts_cache(
            &runtime.shared_accounts_path(),
            &AccountsCache {
                providers: std::collections::BTreeMap::from([(
                    "claude".to_owned(),
                    ProviderRecord {
                        probed_at_ms: 1,
                        ok: true,
                        account: Some(AgentAccount {
                            metered: Some(true),
                            credentials_updated_at_ms: Some(7),
                            ..Default::default()
                        }),
                    },
                )]),
            },
        );
        let snapshot = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
        let mut spawn_attempts = 0;
        refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
            spawn_attempts += 1;
            false
        });
        assert_eq!(spawn_attempts, 1);
        assert_eq!(
            super::super::credits::read_credits_cache(&runtime.shared_credits_path()).entries
                ["claude"]
                .direct_query_claim,
            None
        );
        assert!(
            claim_provider_account_usage(
                &runtime,
                "claude",
                AccountUsageIdentity {
                    credentials_stamp: Some(7),
                    ..Default::default()
                }
            )
            .is_some()
        );
    }

    #[test]
    fn simultaneous_schedulers_spawn_once_per_provider_kind() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        super::super::accounts::write_accounts_cache(
            &runtime.shared_accounts_path(),
            &AccountsCache {
                providers: BTreeMap::from([(
                    "claude".to_owned(),
                    ProviderRecord {
                        probed_at_ms: 1,
                        ok: true,
                        account: Some(AgentAccount {
                            metered: Some(true),
                            ..Default::default()
                        }),
                    },
                )]),
            },
        );
        let snapshot = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
        let spawns = std::sync::atomic::AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    refresh_account_usage_with(&snapshot, &runtime, |_, _, _, _| {
                        spawns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        true
                    });
                });
            }
        });

        assert_eq!(spawns.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn account_usage_segments_fit_strictly_inside_the_renewed_lease() {
        let realtime_segment = crate::agents::codex::app_server::MAX_REALTIME_ACCOUNT_USAGE_DURATION
            + crate::store::lock::LOCK_TIMEOUT;
        let direct_segment =
            crate::agents::credits::OAUTH_HTTP_MAX_DURATION * 2 + crate::store::lock::LOCK_TIMEOUT;

        assert!(realtime_segment < crate::sidebar::timing::ACCOUNT_USAGE_CLAIM_TTL);
        assert!(direct_segment < crate::sidebar::timing::ACCOUNT_USAGE_CLAIM_TTL);
    }
}
