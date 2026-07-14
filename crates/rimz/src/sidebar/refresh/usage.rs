//! Uniform provider account-usage refresh.
//!
//! Every metered provider has two account-usage channels: a *realtime* source
//! (a live session's statusline/app-server/extension reading, folded per kind by
//! the snapshot view) and an *API-query* source — a direct OAuth read of the
//! provider's own quota surface using its own token
//! ([`AgentAdapter::probe_oauth_usage`](crate::agents::AgentAdapter::probe_oauth_usage)).
//!
//! This module owns the producer-side driver that schedules the API-query
//! channel — one loop over the metered provider panels — and the child-side
//! executor that performs the read under the shared single-flight. The actual
//! network read runs in detached `rimz agents refresh-usage` helpers; the
//! producer only decides whether to spawn them. Both halves write the same
//! per-kind `rate_limits.json`/`credits.json` caches the realtime channel does,
//! so the existing fusion reconciles the two sources with nothing keyed across
//! providers.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::OauthUsageProbe;
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{CREDITS_TTL, OAUTH_USAGE_TTL};

use super::credits::{
    ProviderCreditsEntry, account_key_mismatch, merge_provider_credits_entry_if_due,
    read_credits_cache,
};
use super::{SidebarSnapshot, drop_kind_rate_limits, merge_account_rate_limits};

/// Schedule each metered, logged-in provider's API-query account-usage refresh.
/// One uniform loop: gate on the offline override and the OAuth-attempt
/// throttle marker, then spawn the detached helper. Producer-only; the network
/// read runs in the child and single-flights on the shared credits cache.
pub(crate) fn refresh_account_usage(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    refresh_account_usage_with(snapshot, runtime, spawn_usage_refresh);
}

fn refresh_account_usage_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    mut spawn: impl FnMut(&RuntimePaths, &str, bool),
) {
    if crate::agents::credits::oauth_usage_offline() {
        return;
    }
    for panel in &snapshot.providers {
        if !panel.metered {
            continue;
        }
        let kind = panel.kind.as_str();
        if crate::agents::descriptor_by_kind(kind).is_none() {
            continue;
        }
        if !usage_probe_due(runtime, kind) {
            continue;
        }
        spawn(runtime, kind, merge_windows_hint(snapshot, kind));
    }
}

/// Run one kind's API-query channel: single-flight the network read through the
/// credits cache, fold paid usage into `credits.json`, and merge any returned
/// windows into `rate_limits.json` when `merge_windows` is set. Returns whether
/// anything was written — a still-fresh cache entry skips the fetch and returns
/// `false`. The detached refresh child calls this; the producer never does I/O.
pub fn merge_oauth_usage_if_due(runtime: &RuntimePaths, kind: &str, merge_windows: bool) -> bool {
    let Some(adapter) = crate::agents::find_adapter(kind) else {
        return false;
    };
    let stamp = adapter.oauth_credentials_stamp();
    let account_key = adapter.oauth_account_key();
    let prior_account_key = read_credits_cache(&runtime.shared_credits_path())
        .entries
        .get(kind)
        .map(|entry| entry.account_key.clone());
    let mut fetched_windows = None;
    let entry =
        merge_provider_credits_entry_if_due(runtime, kind, stamp, account_key.clone(), || {
            match adapter.probe_oauth_usage() {
                OauthUsageProbe::Found(usage) => {
                    fetched_windows = usage.rate_limits.clone();
                    ProviderCreditsEntry {
                        observed_at_ms: unix_now_ms(),
                        oauth_read_at_ms: unix_now_ms(),
                        auth_settled: false,
                        credentials_stamp: None,
                        account_key: usage.account_key,
                        plan: usage.plan,
                        ok: true,
                        extra_credits: usage.extra_credits,
                        reset_credits: usage.reset_credits,
                    }
                }
                OauthUsageProbe::NoCredentials => ProviderCreditsEntry {
                    observed_at_ms: unix_now_ms(),
                    oauth_read_at_ms: unix_now_ms(),
                    auth_settled: true,
                    credentials_stamp: stamp,
                    account_key: None,
                    plan: None,
                    ok: false,
                    extra_credits: None,
                    reset_credits: None,
                },
                OauthUsageProbe::Failed | OauthUsageProbe::Unsupported => ProviderCreditsEntry {
                    observed_at_ms: unix_now_ms(),
                    oauth_read_at_ms: unix_now_ms(),
                    auth_settled: false,
                    credentials_stamp: None,
                    account_key: None,
                    plan: None,
                    ok: false,
                    extra_credits: None,
                    reset_credits: None,
                },
            }
        });
    let written = entry.is_some();
    if entry.as_ref().is_some_and(|entry| {
        drop_windows_on_account_change(
            runtime,
            kind,
            prior_account_key.as_ref(),
            entry.account_key.as_deref(),
        )
    }) {
        tracing::info!(
            target: crate::observability::BREADCRUMB_TARGET,
            kind,
            "provider account changed; dropping cached windows",
        );
    }
    if merge_windows && let Some(rate_limits) = fetched_windows {
        merge_account_rate_limits(runtime, kind, rate_limits);
    }
    written
}

fn drop_windows_on_account_change(
    runtime: &RuntimePaths,
    kind: &str,
    prior_account_key: Option<&Option<String>>,
    current_account_key: Option<&str>,
) -> bool {
    let changed = prior_account_key
        .is_some_and(|prior| account_key_mismatch(prior.as_deref(), current_account_key));
    if changed {
        drop_kind_rate_limits(runtime, kind);
    }
    changed
}

/// Whether the OAuth windows the API-query channel returns should be merged for
/// this kind. Kinds with fresh realtime windows defer through their descriptor
/// flag; every other kind merges OAuth windows here.
fn merge_windows_hint(snapshot: &SidebarSnapshot, kind: &str) -> bool {
    !crate::agents::descriptor_by_kind(kind).is_some_and(|descriptor| {
        descriptor
            .capabilities
            .realtime_usage
            .windows_defer_to_fresh_realtime
            && has_fresh_realtime_windows(snapshot, kind)
    })
}

/// Whether a live, content-fresh realtime reading already carries this
/// frame's windows, so the authoritative OAuth merge can skip them (the credits
/// read still runs). An idle session may re-emit a days-old payload with a
/// fresh capture stamp, so the capture stamp alone would wrongly shadow truth —
/// the reading's shortest window's passed reset gives the stale payload away.
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
            && now.duration_since(context.observed_at).as_secs() <= CREDITS_TTL.as_secs() as i64
    })
}

/// Throttle one kind's API-query refresh via a marker file under the runtime
/// root: skip when the last attempt is younger than the OAuth TTL, touch it
/// before spawning so a fetch that never publishes still backs off this kind.
pub(crate) fn usage_probe_due(runtime: &RuntimePaths, kind: &str) -> bool {
    let current_stamp =
        crate::agents::find_adapter(kind).and_then(|adapter| adapter.oauth_credentials_stamp());
    usage_probe_due_with_stamp(runtime, kind, current_stamp)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UsageProbeMarker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credentials_stamp: Option<u64>,
}

fn usage_probe_due_with_stamp(
    runtime: &RuntimePaths,
    kind: &str,
    current_stamp: Option<u64>,
) -> bool {
    let path = usage_probe_marker(runtime, kind);
    let age_due = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= OAUTH_USAGE_TTL);
    let stored_stamp = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<UsageProbeMarker>(&bytes).ok())
        .and_then(|marker| marker.credentials_stamp);
    let due = age_due || stored_stamp != current_stamp;
    if due {
        if let Ok(payload) = serde_json::to_vec(&UsageProbeMarker {
            credentials_stamp: current_stamp,
        }) {
            let _ = std::fs::write(&path, payload);
        }
    }
    due
}

pub(crate) fn invalidate_oauth_usage_throttle(runtime: &RuntimePaths, kind: &str) {
    let _ = std::fs::remove_file(usage_probe_marker(runtime, kind));
    crate::sidebar::refresh::credits::invalidate_oauth_read(runtime, kind);
}

pub(crate) fn usage_probe_marker(runtime: &RuntimePaths, kind: &str) -> PathBuf {
    runtime.shared_root.join(format!("usage-probe.{kind}"))
}

fn spawn_usage_refresh(runtime: &RuntimePaths, kind: &str, merge_windows: bool) {
    let exe = crate::proc::rimz_exe();
    let mut cmd = crate::child_process::detached_rimz_command(exe, runtime);
    cmd.args([
        "agents",
        "refresh-usage",
        "--kind",
        kind,
        "--workspace-id",
        runtime.workspace_id.as_str(),
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
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "agents-refresh-usage")
    {
        // Best-effort enrichment on a throttled producer path. The CWD anchor
        // clears the gc'd-worktree ENOENT; a bad RIMZ_BIN/PATH is an
        // environment fact, not a Rimz fault. Keep it at debug! so it never
        // reaches Sentry.
        tracing::debug!(
            workspace = %runtime.workspace_id,
            kind,
            tags.operation = "agents.usage_refresh.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn account usage refresh",
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;

    use crate::agents::lifecycle::TurnPhase;
    use crate::agents::{AgentRateLimits, RateLimitWindow};
    use crate::agents::{AgentState, AgentStatus};
    use crate::ids::{PaneId, WorkspaceId};
    use crate::pane::PaneRef;

    use super::*;

    #[test]
    fn usage_probe_marker_throttles_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        assert!(usage_probe_due(&runtime, "opencode"));
        assert!(!usage_probe_due(&runtime, "opencode"));
        // A different kind has its own marker.
        assert!(usage_probe_due(&runtime, "pi"));

        let old = SystemTime::now()
            .checked_sub(OAUTH_USAGE_TTL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(usage_probe_marker(&runtime, "opencode"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(usage_probe_due(&runtime, "opencode"));
    }

    #[test]
    fn usage_probe_marker_tracks_credential_stamp_and_accepts_legacy_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        assert!(usage_probe_due_with_stamp(&runtime, "codex", Some(1)));
        assert!(!usage_probe_due_with_stamp(&runtime, "codex", Some(1)));
        assert!(usage_probe_due_with_stamp(&runtime, "codex", Some(2)));

        let legacy = usage_probe_marker(&runtime, "claude");
        std::fs::write(&legacy, b"").unwrap();
        assert!(
            !usage_probe_due_with_stamp(&runtime, "claude", None),
            "legacy markers still throttle sources without a file stamp"
        );
        assert!(
            usage_probe_due_with_stamp(&runtime, "claude", Some(7)),
            "a known file stamp bypasses a legacy marker"
        );
    }

    #[test]
    fn identified_owner_drops_unowned_cached_windows() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        merge_account_rate_limits(
            &runtime,
            "codex",
            AgentRateLimits {
                windows: vec![RateLimitWindow {
                    duration_mins: Some(300),
                    used_percentage: Some(50),
                    ..RateLimitWindow::default()
                }],
            },
        );

        assert!(drop_windows_on_account_change(
            &runtime,
            "codex",
            Some(&None),
            Some("acc")
        ));
        assert!(
            !crate::agents::read_rate_limits_cache(&runtime.shared_rate_limits_path())
                .windows
                .contains_key("codex")
        );
    }

    #[test]
    fn realtime_windows_merge_only_defers_to_recent_statusline_windows() {
        let now = Timestamp::now();
        let fresh = snapshot_with_agent(statusline_agent("fresh", now));
        assert!(has_fresh_realtime_windows(&fresh, "claude"));
        assert!(!merge_windows_hint(&fresh, "claude"));
        assert!(merge_windows_hint(&fresh, "codex"));

        let stale_at = now
            .checked_sub(jiff::SignedDuration::from_secs(
                CREDITS_TTL.as_secs() as i64 + 1,
            ))
            .unwrap();
        let stale = snapshot_with_agent(statusline_agent("stale", stale_at));
        assert!(!has_fresh_realtime_windows(&stale, "claude"));
        assert!(merge_windows_hint(&stale, "claude"));

        let mut no_windows = statusline_agent("none", now);
        no_windows.context.as_mut().unwrap().rate_limits = None;
        assert!(!has_fresh_realtime_windows(
            &snapshot_with_agent(no_windows),
            "claude"
        ));

        // The idle-session trap: a fresh capture stamp over a stale payload. The
        // 5h window already reset (so the content is stale) even though
        // `observed_at` is now and the longer 7d window's reset is still future.
        let mut content_stale = statusline_agent("content-stale", now);
        content_stale.context.as_mut().unwrap().rate_limits = Some(AgentRateLimits {
            windows: vec![
                RateLimitWindow {
                    used_percentage: Some(57),
                    resets_at: now.checked_sub(jiff::SignedDuration::from_hours(1)).ok(),
                    duration_mins: Some(300),
                    ..Default::default()
                },
                RateLimitWindow {
                    used_percentage: Some(59),
                    resets_at: now.checked_add(jiff::SignedDuration::from_hours(48)).ok(),
                    duration_mins: Some(7 * 24 * 60),
                    ..Default::default()
                },
            ],
        });
        assert!(
            !has_fresh_realtime_windows(&snapshot_with_agent(content_stale), "claude"),
            "a fresh capture stamp can't rescue a payload whose 5h window already reset"
        );
    }

    #[test]
    fn codex_with_live_root_session_still_schedules_oauth_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let now = Timestamp::now();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace, vec![codex_agent("codex-1", now)], now);
        snapshot.providers = vec![crate::sidebar::test_support::provider_panel(
            "codex",
            Vec::new(),
        )];

        let mut spawned = Vec::new();
        refresh_account_usage_with(&snapshot, &runtime, |_, kind, merge_windows| {
            spawned.push((kind.to_owned(), merge_windows));
        });

        assert_eq!(spawned, vec![("codex".to_owned(), true)]);
        assert!(
            usage_probe_marker(&runtime, "codex").exists(),
            "a live root session no longer suppresses the OAuth usage helper"
        );
    }

    fn snapshot_with_agent(agent: AgentState) -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/usage-refresh")),
            vec![agent],
            Timestamp::now(),
        )
    }

    fn statusline_agent(id: &str, observed_at: Timestamp) -> AgentState {
        let mut context = crate::store::agent_context::empty_context("claude", observed_at);
        context.rate_limits = Some(AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(12),
                resets_at: None,
                duration_mins: Some(300),
                ..Default::default()
            }],
        });
        base_agent(id, "claude", Some(context), observed_at)
    }

    fn codex_agent(id: &str, observed_at: Timestamp) -> AgentState {
        base_agent(id, "codex", None, observed_at)
    }

    fn base_agent(
        id: &str,
        kind: &str,
        context: Option<crate::agents::AgentContext>,
        observed_at: Timestamp,
    ) -> AgentState {
        AgentState {
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            pane: Some(pane()),
            context,
            ..crate::testkit::agent_state(kind, id, observed_at)
        }
    }

    fn pane() -> PaneRef {
        let mut pane = PaneRef::from_id(PaneId::parse("tmux:%1").unwrap());
        pane.session_name = "test".to_owned();
        pane.view_id = Some("tab-1".to_owned());
        pane.command = Some("claude".to_owned());
        pane
    }
}
