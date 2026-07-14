//! Uniform provider account-usage refresh.
//!
//! Sidebar producers claim due direct reads in `credits.json` before spawning a
//! helper. Provider calls run outside cache locks; a matching nonce alone may
//! publish the result. Realtime and direct readings share one snapshot shape
//! and one cache-publication path.

use uuid::Uuid;

use crate::agents::{AccountUsageIdentity, AccountUsageSnapshot, ProviderAccountScope};
use crate::{RuntimePaths, SidebarSnapshot};

use super::accounts::cached_account_usage_hint;
use super::credits::{
    account_usage_claim_matches, cancel_provider_account_usage_claim, claim_provider_account_usage,
    complete_provider_account_usage, merge_provider_realtime_usage,
};
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
        let kind = panel.kind.as_str();
        let Some(adapter) = crate::agents::find_adapter(kind) else {
            continue;
        };
        let identity = scheduling_identity(runtime, kind, adapter);
        let Some(claim_id) = claim_provider_account_usage(runtime, kind, identity) else {
            continue;
        };
        if !spawn(runtime, kind, merge_windows_hint(snapshot, kind), claim_id) {
            cancel_provider_account_usage_claim(runtime, kind, claim_id);
        }
    }
}

fn scheduling_identity(
    runtime: &RuntimePaths,
    kind: &str,
    adapter: &dyn crate::agents::AgentAdapter,
) -> AccountUsageIdentity {
    let cached_hint = cached_account_usage_hint(runtime, kind);
    if let Some((scope, Some(credentials_stamp))) = cached_hint.as_ref() {
        return AccountUsageIdentity {
            scope: scope.clone(),
            credentials_stamp: Some(*credentials_stamp),
            account_key: None,
        };
    }
    let mut identity = adapter.account_usage_identity();
    if let Some((scope, stamp)) = cached_hint {
        identity.scope = scope;
        identity.credentials_stamp = identity.credentials_stamp.or(stamp);
    }
    identity
}

/// Run one producer-created claim. The helper validates the nonce before any
/// provider call; late or superseded workers leave both caches untouched.
pub fn refresh_claimed_account_usage(
    runtime: &RuntimePaths,
    kind: &str,
    claim_id: Uuid,
    merge_windows: bool,
) -> bool {
    if crate::agents::credits::oauth_usage_offline() {
        cancel_provider_account_usage_claim(runtime, kind, claim_id);
        return false;
    }
    if !account_usage_claim_matches(runtime, kind, claim_id) {
        return false;
    }
    let Some(adapter) = crate::agents::find_adapter(kind) else {
        cancel_provider_account_usage_claim(runtime, kind, claim_id);
        return false;
    };
    let realtime = adapter.probe_realtime_account_usage(runtime);
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
        return false;
    }
    let mut wrote = false;
    if let Some(usage) = realtime {
        wrote |= publish_account_usage_snapshot(
            runtime,
            kind,
            ProviderAccountScope::KindWide,
            usage,
            true,
        );
    }
    wrote |= complete_direct_account_usage(
        runtime,
        kind,
        claim_id,
        adapter.probe_account_usage(),
        publish_direct_windows,
    );
    wrote
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

/// Claim and execute a direct read in-process. Codex synchronous refresh paths
/// use this instead of maintaining a separate cadence.
pub fn merge_account_usage_if_due(runtime: &RuntimePaths, kind: &str, merge_windows: bool) -> bool {
    let Some(adapter) = crate::agents::find_adapter(kind) else {
        return false;
    };
    let identity = scheduling_identity(runtime, kind, adapter);
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

pub(crate) fn invalidate_oauth_usage_throttle(runtime: &RuntimePaths, kind: &str) {
    super::credits::invalidate_oauth_read(runtime, kind);
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

    use crate::agents::{AgentAccount, AgentRateLimits, RateLimitWindow, read_rate_limits_cache};
    use crate::ids::WorkspaceId;
    use crate::sidebar::refresh::accounts::{AccountsCache, ProviderRecord};
    use crate::sidebar::refresh::credits::{CreditsCache, ProviderCreditsEntry};
    use crate::sidebar::test_support::{provider_panel, snapshot_with_panels};

    use super::*;

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
}
