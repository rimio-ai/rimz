use std::path::PathBuf;
use std::time::SystemTime;

use crate::RuntimePaths;
use crate::config::AccountsConfig;
use crate::sidebar::timing::CREDITS_TTL;

use super::SidebarSnapshot;

/// Refresh provider OAuth account-usage surfaces from the producer. The actual
/// network read runs in detached helpers; this function only decides whether to
/// spawn them.
pub(super) fn refresh_oauth_usage(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    accounts: &AccountsConfig,
) {
    refresh_claude_oauth_usage(snapshot, runtime, accounts);
}

fn refresh_claude_oauth_usage(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    accounts: &AccountsConfig,
) {
    if !accounts.oauth_usage || crate::agents::credits::oauth_usage_offline() {
        return;
    }
    if !snapshot
        .providers
        .iter()
        .any(|panel| panel.kind == "claude" && panel.metered)
    {
        return;
    }
    if crate::sidebar::enrich::provider_credits_entry_fresh(runtime, "claude") {
        return;
    }
    if !oauth_usage_probe_due(runtime, "claude") {
        return;
    }
    let merge_windows = !has_fresh_claude_statusline_windows(snapshot);
    let agent_version = snapshot
        .providers
        .iter()
        .find(|panel| panel.kind == "claude")
        .and_then(|panel| panel.version.as_deref());
    spawn_claude_usage_refresh(runtime, merge_windows, agent_version);
}

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
        // A live session's statusline windows suppress the authoritative OAuth
        // merge only when both the capture is recent *and* the content is fresh.
        // An idle session re-emits a days-old payload with a fresh `observed_at`,
        // so the capture stamp alone would wrongly shadow truth; the reading's
        // shortest window's passed reset gives the stale payload away.
        !limits.windows.is_empty()
            && !limits.content_stale_at(now)
            && now.duration_since(context.observed_at).as_secs() <= CREDITS_TTL.as_secs() as i64
    })
}

pub(crate) fn oauth_usage_probe_due(runtime: &RuntimePaths, kind: &str) -> bool {
    let path = oauth_usage_probe_marker(runtime, kind);
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

pub(crate) fn oauth_usage_probe_marker(runtime: &RuntimePaths, kind: &str) -> PathBuf {
    runtime
        .shared_root
        .join(format!("oauth-usage-probe.{kind}"))
}

fn spawn_claude_usage_refresh(
    runtime: &RuntimePaths,
    merge_windows: bool,
    agent_version: Option<&str>,
) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(
                workspace = %runtime.workspace_id,
                tags.operation = "claude.usage_refresh.locate_exe",
                error = &err as &dyn std::error::Error,
                "sidebar: cannot locate rimz to refresh claude usage",
            );
            return;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "claude",
        "refresh-usage",
        "--workspace-id",
        runtime.workspace_id.as_str(),
    ]);
    if merge_windows {
        cmd.arg("--merge-windows");
    }
    if let Some(version) = agent_version.filter(|version| !version.trim().is_empty()) {
        cmd.args(["--agent-version", version]);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        merge_windows,
        "sidebar: spawning claude usage refresh",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "claude-refresh-usage")
    {
        tracing::warn!(
            workspace = %runtime.workspace_id,
            tags.operation = "claude.usage_refresh.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn claude usage refresh",
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
    fn oauth_probe_marker_throttles_per_kind() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        assert!(oauth_usage_probe_due(&runtime, "claude"));
        assert!(!oauth_usage_probe_due(&runtime, "claude"));
        assert!(oauth_usage_probe_due(&runtime, "codex"));

        let old = SystemTime::now()
            .checked_sub(CREDITS_TTL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(oauth_usage_probe_marker(&runtime, "claude"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(oauth_usage_probe_due(&runtime, "claude"));
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
        // Without the content-staleness check this would wrongly suppress the
        // authoritative OAuth window merge.
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

        fn snapshot_with_agent(agent: AgentState) -> SidebarSnapshot {
            SidebarSnapshot::build_with_agents(
                WorkspaceId::from_project_root(std::path::Path::new("/tmp/oauth-windows")),
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
            AgentState {
                agent_id: AgentSessionId::from(id),
                kind: AgentKind::new_unchecked("claude"),
                name: None,
                kind_ordinal: None,
                alias: None,
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
                fresh_input_tokens: None,
                output_tokens: None,
                todo_done: None,
                todo_total: None,
                context: Some(context),
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
}
