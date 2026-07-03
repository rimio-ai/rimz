//! Producer-owned live-session context refresh.
//!
//! The elected sidebar producer asks each live root session's adapter for cheap
//! transcript-tail refreshes and optional detached rich-context helpers through
//! the same trigger seams hooks use.

use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::{LifecycleRefreshCtx, LocalContextRefreshCtx, RefreshSpawn, RefreshTrigger};
use crate::sidebar::timing::{
    SESSION_PROBE_MARKER_PREFIX, SESSION_PROBE_MARKER_TTL, SESSION_REFRESH_INTERVAL,
};

use super::SidebarSnapshot;

/// Refresh every live root session's adapter-owned context sidecar from the
/// producer. Inline transcript reads run first with their adapter stat gate;
/// detached helpers run on a coarse per-session cadence for richer realtime
/// channels.
pub(crate) fn refresh_live_sessions(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    for refresh in live_session_refreshes(snapshot) {
        refresh_session_transcript_context(
            runtime,
            &refresh.kind,
            &refresh.session_id,
            refresh.model_hint.as_deref(),
        );
        let spawn = session_context_refresh_spawn(
            runtime,
            &refresh.kind,
            &refresh.session_id,
            refresh.model_hint.as_deref(),
        );
        if let Some(spawn) = spawn
            && session_probe_due(runtime, &refresh.kind, &refresh.session_id)
        {
            spawn_session_context_refresh(runtime, &refresh.kind, &refresh.session_id, spawn);
        }
    }
    reap_stale_session_probe_markers(runtime);
}

/// Refresh one session's local transcript/rollout context into its sidecar and
/// wake every renderer. Adapter no-op and stat-gated no-change reads are free,
/// so producer ticks and transcript watchers can call this freely.
pub fn refresh_session_transcript_context(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    model_hint: Option<&str>,
) {
    let Some(adapter) = crate::agents::find_adapter(kind) else {
        return;
    };
    let prior = crate::ledger::agent_context::read_one(runtime, kind, session_id);
    let ctx = LocalContextRefreshCtx {
        agent_id: session_id,
        model_hint,
        prior_transcript_path: prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior_transcript_stat: prior
            .as_ref()
            .and_then(|record| record.transcript_stat.as_ref()),
    };
    let refresh = adapter.local_context_refresh(RefreshTrigger::Tick, &ctx);
    let Some(refresh) = refresh else {
        return;
    };
    if let Err(err) = crate::ledger::agent_context::merge_local_context(
        runtime,
        kind,
        session_id,
        prior,
        refresh,
        Timestamp::now(),
    ) {
        tracing::warn!(
            kind,
            session = %session_id,
            tags.operation = "session.transcript_merge",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to merge session transcript context",
        );
        return;
    }
    let _ = crate::ledger::wakeup::wake_sidebars(runtime);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveSessionRefresh {
    pub kind: String,
    pub session_id: String,
    pub model_hint: Option<String>,
}

pub(crate) fn live_session_refreshes(snapshot: &SidebarSnapshot) -> Vec<LiveSessionRefresh> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| crate::agents::find_adapter(agent.kind.as_str()).is_some())
        .map(|agent| LiveSessionRefresh {
            kind: agent.kind.as_str().to_owned(),
            session_id: agent.agent_id.to_string(),
            model_hint: agent
                .model
                .clone()
                .or_else(|| agent.context.as_ref().and_then(|ctx| ctx.model_id.clone())),
        })
        .collect()
}

/// Throttle one session's detached context refresh via a marker file under the
/// runtime root: skip when the last attempt is younger than the interval, touch
/// it before spawning.
pub(crate) fn session_probe_due(runtime: &RuntimePaths, kind: &str, session_id: &str) -> bool {
    let path = session_probe_marker(runtime, kind, session_id);
    let due = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= SESSION_REFRESH_INTERVAL);
    if due {
        // Touch first so a fetch that never publishes still backs off this target.
        let _ = std::fs::write(&path, b"");
    }
    due
}

pub(crate) fn session_probe_marker(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"session-context");
    hasher.update([0]);
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    runtime
        .shared_root
        .join(format!("{SESSION_PROBE_MARKER_PREFIX}{}", &digest[..32]))
}

fn reap_stale_session_probe_markers(runtime: &RuntimePaths) {
    let Ok(entries) = std::fs::read_dir(&runtime.shared_root) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(SESSION_PROBE_MARKER_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= SESSION_PROBE_MARKER_TTL);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn session_context_refresh_spawn(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    model_hint: Option<&str>,
) -> Option<RefreshSpawn> {
    let adapter = crate::agents::find_adapter(kind)?;
    let refresh_ctx = LifecycleRefreshCtx {
        agent_id: session_id,
        workspace_id: runtime.workspace_id.as_str(),
        model_hint,
        server_url: None,
    };
    adapter.context_refresh_spawn(RefreshTrigger::Tick, &refresh_ctx)
}

/// Spawn the detached, fresh-stdio helper an adapter requests for one active
/// session. Best-effort: a spawn failure is logged and dropped.
fn spawn_session_context_refresh(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    spawn: RefreshSpawn,
) {
    let exe = crate::proc::rimz_exe();
    let mut cmd = crate::child_process::detached_rimz_command(exe, runtime);
    cmd.args(spawn.args);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        kind,
        session = %session_id,
        "sidebar: spawning session context refresh",
    );
    if let Err(err) =
        crate::child_process::spawn_detached_reaped(&mut cmd, "session-refresh-context")
    {
        // Best-effort enrichment on a per-frame path. The CWD anchor clears the
        // gc'd-worktree ENOENT; a bad RIMZ_BIN/PATH is an environment fact, not
        // a Rimz fault. Keep it at debug! so it never reaches Sentry.
        tracing::debug!(
            kind,
            session = %session_id,
            workspace = %runtime.workspace_id,
            tags.operation = "session.context_refresh.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn session context refresh",
        );
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::RuntimePaths;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::{
        provider_panel, rl_window, root_agent, snapshot_with_panels,
    };

    #[test]
    fn live_session_refreshes_target_live_root_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());

        let mut active_with_windows = snapshot_with_panels(
            workspace.clone(),
            vec![provider_panel("codex", vec![rl_window(42, None)])],
        );
        active_with_windows
            .agents
            .push(root_agent("codex", "sess-active", Some("gpt-5.5-codex")));
        active_with_windows
            .agents
            .push(root_agent("claude", "claude-active", Some("opus")));
        assert_eq!(
            live_session_refreshes(&active_with_windows),
            vec![
                LiveSessionRefresh {
                    kind: "codex".to_owned(),
                    session_id: "sess-active".to_owned(),
                    model_hint: Some("gpt-5.5-codex".to_owned()),
                },
                LiveSessionRefresh {
                    kind: "claude".to_owned(),
                    session_id: "claude-active".to_owned(),
                    model_hint: Some("opus".to_owned()),
                }
            ],
            "live root sessions refresh their sidecars even when the dashboard already has windows"
        );

        // An idle metered account has no live session to refresh here — the
        // uniform usage driver covers its account-scoped read while idle.
        let idle_metered =
            snapshot_with_panels(workspace.clone(), vec![provider_panel("codex", Vec::new())]);
        assert!(
            live_session_refreshes(&idle_metered).is_empty(),
            "an idle account has no session sidecar to refresh"
        );

        let mut active_no_model =
            snapshot_with_panels(workspace, vec![provider_panel("codex", Vec::new())]);
        active_no_model
            .agents
            .push(root_agent("codex", "sess-active", None));
        assert_eq!(
            live_session_refreshes(&active_no_model),
            vec![LiveSessionRefresh {
                kind: "codex".to_owned(),
                session_id: "sess-active".to_owned(),
                model_hint: None,
            }],
            "a live sidecar refreshes even with no model hint"
        );
    }

    /// The per-session throttle marker gates the app-server refresh: the first call
    /// is due (and touches the marker), the immediate next is not.
    #[test]
    fn session_probe_throttles_per_kind_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        assert!(session_probe_due(&runtime, "codex", "sess/one"));
        assert!(
            !session_probe_due(&runtime, "codex", "sess/one"),
            "a freshly-stamped session backs off"
        );
        assert!(
            session_probe_due(&runtime, "codex", "sess/two"),
            "a different session has its own marker"
        );
        assert!(
            session_probe_due(&runtime, "claude", "sess/one"),
            "a different kind has its own marker"
        );

        let old = SystemTime::now()
            .checked_sub(SESSION_REFRESH_INTERVAL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(session_probe_marker(&runtime, "codex", "sess/one"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(
            session_probe_due(&runtime, "codex", "sess/one"),
            "the session becomes due again after the 60s interval"
        );
    }

    #[test]
    fn reap_removes_stale_session_probe_markers() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        let stale_session = runtime.shared_root.join(format!(
            "{SESSION_PROBE_MARKER_PREFIX}00000000000000000000000000000000"
        ));
        let fresh_session = runtime.shared_root.join(format!(
            "{SESSION_PROBE_MARKER_PREFIX}11111111111111111111111111111111"
        ));
        let accounts = runtime.shared_root.join("accounts.json");
        for path in [&stale_session, &fresh_session, &accounts] {
            std::fs::write(path, b"").unwrap();
        }
        let old = SystemTime::now()
            .checked_sub(SESSION_PROBE_MARKER_TTL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(&stale_session)
            .unwrap()
            .set_modified(old)
            .unwrap();

        reap_stale_session_probe_markers(&runtime);

        assert!(!stale_session.exists());
        assert!(fresh_session.exists());
        assert!(accounts.exists());
    }

    #[test]
    fn unsupported_tick_adapter_writes_no_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        refresh_session_transcript_context(&runtime, "claude", "sess-1", Some("opus"));

        assert!(crate::ledger::agent_context::read_all(&runtime).is_empty());
    }

    #[test]
    fn transcript_backstop_is_stat_gated() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let path = dir.path().join("rollout-session.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
             \"model_context_window\":100}}}\n",
        )
        .unwrap();

        let mut record = crate::ledger::agent_context::new_record(
            "codex",
            "sess-1",
            crate::ledger::agent_context::empty_context("codex", Timestamp::now()),
        );
        record.transcript_path = Some(path.to_string_lossy().into_owned());
        crate::ledger::agent_context::write_record(&runtime, &record).unwrap();

        refresh_session_transcript_context(&runtime, "codex", "sess-1", Some("gpt-5"));
        let first = crate::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
        // The sidecar carries the derivation inputs (window + current usage), not a
        // baked percentage; the gauge derives 50% (50 of 100) downstream.
        let first_tokens = first
            .context
            .tokens
            .as_ref()
            .expect("first refresh writes tokens");
        assert_eq!(first_tokens.context_window_size, Some(100));
        assert_eq!(
            first_tokens
                .current_usage
                .as_ref()
                .and_then(|usage| usage.input_tokens),
            Some(50)
        );
        let observed_at = first.context.observed_at;
        let stat = first.transcript_stat;

        refresh_session_transcript_context(&runtime, "codex", "sess-1", Some("gpt-5"));
        let second = crate::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
        assert_eq!(second.context.observed_at, observed_at);
        assert_eq!(second.transcript_stat, stat);

        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(
                b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
              \"last_token_usage\":{\"input_tokens\":80,\"total_tokens\":90},\
              \"model_context_window\":100}}}\n",
            )
            .unwrap();
        refresh_session_transcript_context(&runtime, "codex", "sess-1", Some("gpt-5"));
        let third = crate::ledger::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
        assert_eq!(
            third
                .context
                .tokens
                .as_ref()
                .and_then(|t| t.current_usage.as_ref())
                .and_then(|usage| usage.input_tokens),
            Some(80)
        );
        assert_ne!(third.transcript_stat, stat);
    }
}
