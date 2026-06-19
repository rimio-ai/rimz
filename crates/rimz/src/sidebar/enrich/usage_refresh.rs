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

use crate::agents::OauthUsageProbe;
use crate::sidebar::cache::unix_now_ms;
use crate::sidebar::timing::CREDITS_TTL;
use crate::{RuntimePaths, SidebarProviderPanel};

use super::credits::{ProviderCreditsEntry, merge_provider_credits_entry_if_due};
use super::{SidebarSnapshot, merge_account_rate_limits};

/// Schedule each metered, logged-in provider's API-query account-usage refresh.
/// One uniform loop: gate on the offline override, skip a kind whose credits cache
/// is still fresh or whose throttle marker has not aged out, then spawn the
/// detached helper. Producer-only; the network read runs in the child.
pub(super) fn refresh_account_usage(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    if crate::agents::credits::oauth_usage_offline() {
        return;
    }
    for panel in &snapshot.providers {
        if !panel.metered {
            continue;
        }
        let kind = panel.kind.as_str();
        // Codex's live root session refreshes its app-server windows on turn
        // boundaries (`codex_refresh.rs`), so an account-scoped fetch here would
        // double-hit the app-server. The uniform driver covers codex only while
        // it is idle; every other kind has no such session-scoped path.
        if kind == "codex" && has_live_root_session(snapshot, "codex") {
            continue;
        }
        if super::credits::provider_credits_entry_fresh(runtime, kind) {
            continue;
        }
        if !usage_probe_due(runtime, kind) {
            continue;
        }
        spawn_usage_refresh(runtime, kind, merge_windows_hint(snapshot, panel));
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
    let mut fetched_windows = None;
    let entry =
        merge_provider_credits_entry_if_due(runtime, kind, || match adapter.probe_oauth_usage() {
            OauthUsageProbe::Found(usage) => {
                fetched_windows = usage.rate_limits.clone();
                ProviderCreditsEntry {
                    observed_at_ms: unix_now_ms(),
                    ok: true,
                    extra_credits: usage.extra_credits,
                }
            }
            OauthUsageProbe::NoCredentials
            | OauthUsageProbe::Failed
            | OauthUsageProbe::Unsupported => ProviderCreditsEntry {
                observed_at_ms: unix_now_ms(),
                ok: false,
                extra_credits: None,
            },
        });
    let written = entry.is_some();
    if merge_windows && let Some(rate_limits) = fetched_windows {
        merge_account_rate_limits(runtime, kind, rate_limits);
    }
    written
}

/// Whether the OAuth windows the API-query channel returns should be merged for
/// this kind. Claude defers to a fresh live statusline (its realtime channel);
/// pi/opencode own their windows through OAuth, and codex decides inside its
/// handler arm (app-server first), so every non-claude kind merges.
fn merge_windows_hint(snapshot: &SidebarSnapshot, panel: &SidebarProviderPanel) -> bool {
    match panel.kind.as_str() {
        "claude" => !has_fresh_claude_statusline_windows(snapshot),
        _ => true,
    }
}

/// Whether a live, content-fresh Claude statusline reading already carries this
/// frame's windows, so the authoritative OAuth merge can skip them (the credits
/// read still runs). An idle session re-emits a days-old payload with a fresh
/// capture stamp, so the capture stamp alone would wrongly shadow truth — the
/// reading's shortest window's passed reset gives the stale payload away.
fn has_fresh_claude_statusline_windows(snapshot: &SidebarSnapshot) -> bool {
    let now = snapshot.now;
    snapshot.agents.iter().any(|agent| {
        if agent.kind != "claude" || agent.parent_agent_id.is_some() {
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

fn has_live_root_session(snapshot: &SidebarSnapshot, kind: &str) -> bool {
    snapshot.agents.iter().any(|agent| {
        agent.kind == kind && agent.parent_agent_id.is_none() && !agent.agent_id.is_empty()
    })
}

/// Throttle one kind's API-query refresh via a marker file under the runtime
/// root: skip when the last attempt is younger than the credits TTL, touch it
/// before spawning so a fetch that never publishes still backs off this kind.
pub(crate) fn usage_probe_due(runtime: &RuntimePaths, kind: &str) -> bool {
    let path = usage_probe_marker(runtime, kind);
    let due = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= CREDITS_TTL);
    if due {
        let _ = std::fs::write(&path, b"");
    }
    due
}

pub(crate) fn usage_probe_marker(runtime: &RuntimePaths, kind: &str) -> PathBuf {
    runtime.shared_root.join(format!("usage-probe.{kind}"))
}

fn spawn_usage_refresh(runtime: &RuntimePaths, kind: &str, merge_windows: bool) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(
                workspace = %runtime.workspace_id,
                kind,
                tags.operation = "agents.usage_refresh.locate_exe",
                error = &err as &dyn std::error::Error,
                "sidebar: cannot locate rimz to refresh account usage",
            );
            return;
        }
    };
    let mut cmd = std::process::Command::new(exe);
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
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind,
        merge_windows,
        "sidebar: spawning account usage refresh",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "agents-refresh-usage")
    {
        tracing::warn!(
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
    use crate::feed::{AgentState, AgentStatus, PaneRef};
    use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};

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
            .checked_sub(CREDITS_TTL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(usage_probe_marker(&runtime, "opencode"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(usage_probe_due(&runtime, "opencode"));
    }

    #[test]
    fn claude_windows_merge_only_defers_to_recent_statusline_windows() {
        let now = Timestamp::now();
        let fresh = snapshot_with_agent(statusline_agent("fresh", now));
        assert!(has_fresh_claude_statusline_windows(&fresh));

        let stale_at = now
            .checked_sub(jiff::SignedDuration::from_secs(
                CREDITS_TTL.as_secs() as i64 + 1,
            ))
            .unwrap();
        let stale = snapshot_with_agent(statusline_agent("stale", stale_at));
        assert!(!has_fresh_claude_statusline_windows(&stale));

        let mut no_windows = statusline_agent("none", now);
        no_windows.context.as_mut().unwrap().rate_limits = None;
        assert!(!has_fresh_claude_statusline_windows(&snapshot_with_agent(
            no_windows
        )));

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
            !has_fresh_claude_statusline_windows(&snapshot_with_agent(content_stale)),
            "a fresh capture stamp can't rescue a payload whose 5h window already reset"
        );
    }

    #[test]
    fn codex_with_live_root_session_is_skipped() {
        let now = Timestamp::now();
        let snapshot = snapshot_with_agent(codex_agent("codex-1", now));
        assert!(has_live_root_session(&snapshot, "codex"));
        // A pi session does not satisfy the codex guard.
        assert!(!has_live_root_session(&snapshot, "pi"));
        let empty = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/usage-refresh")),
            Vec::new(),
            Vec::new(),
            now,
        );
        assert!(!has_live_root_session(&empty, "codex"));
    }

    fn snapshot_with_agent(agent: AgentState) -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/usage-refresh")),
            Vec::new(),
            vec![agent],
            Timestamp::now(),
        )
    }

    fn statusline_agent(id: &str, observed_at: Timestamp) -> AgentState {
        let mut context = crate::ledger::agent_context::empty_context("claude", observed_at);
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
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            pane: Some(pane()),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: observed_at,
            last_activity: observed_at,
            registered_at: Some(observed_at),
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
