use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::codex;
use crate::sidebar::timing::{
    CODEX_PROBE_MARKER_PREFIX, CODEX_PROBE_MARKER_TTL, CODEX_RATE_LIMIT_REFRESH_INTERVAL,
};

use super::SidebarSnapshot;

/// Refresh each live/root Codex session's app-server-owned budget/account
/// sidecar from the producer. A session first refreshes its transcript-derived
/// tokens/cost in process with a stat gate, then the detached helper refreshes
/// app-server-owned fields on the coarse per-session cadence so a long-running
/// turn does not wait for the next turn boundary to repaint. The idle,
/// account-scoped read lives in the uniform `usage_refresh` driver.
pub(super) fn refresh_codex_sessions(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    for refresh in codex_session_refreshes(snapshot) {
        refresh_codex_transcript_context(
            runtime,
            &refresh.session_id,
            refresh.model_hint.as_deref(),
        );
        if codex_session_probe_due(runtime, &refresh.session_id) {
            spawn_codex_context_refresh(
                runtime,
                &refresh.session_id,
                refresh.model_hint.as_deref(),
            );
        }
    }
    reap_stale_codex_probe_markers(runtime);
}

/// Refresh one Codex session's transcript-derived tokens/cost into its context
/// sidecar and wake every renderer. Stat-gated: an unchanged rollout tail is a
/// no-op, so every trigger — the producer tick here, the renderer's transcript
/// watcher (`sidebar_pane::app::transcript_watch`) — can fire freely.
pub fn refresh_codex_transcript_context(
    runtime: &RuntimePaths,
    session_id: &str,
    model_hint: Option<&str>,
) {
    let prior = crate::ledger::agent_context::read_one(runtime, "codex", session_id);
    let refresh = codex::refresh_transcript_context(
        session_id,
        model_hint,
        prior
            .as_ref()
            .and_then(|record| record.context.effort.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior
            .as_ref()
            .and_then(|record| record.transcript_stat.as_ref()),
    );
    let Some(refresh) = refresh else {
        return;
    };
    if let Err(err) = crate::ledger::agent_context::merge_local_context(
        runtime,
        "codex",
        session_id,
        prior,
        refresh,
        Timestamp::now(),
    ) {
        tracing::warn!(
            session = %session_id,
            tags.operation = "codex.transcript_merge",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to merge codex transcript context",
        );
        return;
    }
    let _ = crate::ledger::wakeup::wake_sidebars(runtime);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSessionRefresh {
    pub session_id: String,
    pub model_hint: Option<String>,
}

pub(crate) fn codex_session_refreshes(snapshot: &SidebarSnapshot) -> Vec<CodexSessionRefresh> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind == "codex" && agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .map(|agent| CodexSessionRefresh {
            session_id: agent.agent_id.to_string(),
            model_hint: agent
                .model
                .clone()
                .or_else(|| agent.context.as_ref().and_then(|ctx| ctx.model_id.clone())),
        })
        .collect()
}

/// Throttle one Codex session's app-server context refresh via a marker file
/// under the runtime root: skip when the last attempt is younger than the
/// interval, touch it before spawning. Windows move on the scale of minutes, so
/// a one-minute gate keeps a slow/unreachable app-server from spawning a helper
/// every frame while still updating during long-running turns.
pub(crate) fn codex_session_probe_due(runtime: &RuntimePaths, session_id: &str) -> bool {
    let path = codex_session_probe_marker(runtime, session_id);
    let due = std::fs::metadata(&path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= CODEX_RATE_LIMIT_REFRESH_INTERVAL);
    if due {
        // Touch first so a fetch that never publishes still backs off this target.
        let _ = std::fs::write(&path, b"");
    }
    due
}

pub(crate) fn codex_session_probe_marker(runtime: &RuntimePaths, session_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-session");
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    runtime
        .shared_root
        .join(format!("{CODEX_PROBE_MARKER_PREFIX}{}", &digest[..32]))
}

fn reap_stale_codex_probe_markers(runtime: &RuntimePaths) {
    let Ok(entries) = std::fs::read_dir(&runtime.shared_root) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(CODEX_PROBE_MARKER_PREFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= CODEX_PROBE_MARKER_TTL);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Spawn the detached, fresh-stdio helper that refreshes one active Codex
/// session's app-server-owned `AgentContext` fields. Transcript tokens/cost are
/// refreshed in process before this helper is considered. Best-effort: a spawn
/// failure is logged and dropped — the dashboard keeps the prior reading until
/// the next due frame.
fn spawn_codex_context_refresh(runtime: &RuntimePaths, session_id: &str, model_hint: Option<&str>) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(
                session = %session_id,
                workspace = %runtime.workspace_id,
                tags.operation = "codex.context_refresh.locate_exe",
                error = &err as &dyn std::error::Error,
                "sidebar: cannot locate rimz to refresh codex context",
            );
            return;
        }
    };
    let mut cmd = super::detached_rimz_command(exe, runtime);
    cmd.args([
        "codex",
        "refresh-context",
        "--session-id",
        session_id,
        "--workspace-id",
        runtime.workspace_id.as_str(),
    ]);
    if let Some(model) = model_hint {
        cmd.args(["--model", model]);
    }
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        session = %session_id,
        "sidebar: spawning codex context refresh",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "codex-refresh-context")
    {
        // Best-effort enrichment on a per-frame path. The CWD anchor clears the
        // gc'd-worktree ENOENT; a genuinely missing/replaced `rimz` binary
        // (upgrade-during-run) still fails here — an environment fact, not a
        // Rimz fault. Keep it at debug! so it never reaches Sentry.
        tracing::debug!(
            session = %session_id,
            workspace = %runtime.workspace_id,
            tags.operation = "codex.context_refresh.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn codex context refresh",
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
    fn codex_session_refreshes_target_live_root_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());

        let mut active_with_windows = snapshot_with_panels(
            workspace.clone(),
            vec![provider_panel("codex", vec![rl_window(42, None)])],
        );
        active_with_windows
            .agents
            .push(root_agent("codex", "sess-active", Some("gpt-5.5-codex")));
        assert_eq!(
            codex_session_refreshes(&active_with_windows),
            vec![CodexSessionRefresh {
                session_id: "sess-active".to_owned(),
                model_hint: Some("gpt-5.5-codex".to_owned()),
            }],
            "active Codex sessions refresh their sidecars even when the dashboard already has windows"
        );

        // An idle metered Codex account has no live session to refresh here — the
        // uniform usage driver covers its account-scoped read while idle.
        let idle_metered =
            snapshot_with_panels(workspace.clone(), vec![provider_panel("codex", Vec::new())]);
        assert!(
            codex_session_refreshes(&idle_metered).is_empty(),
            "an idle account has no session sidecar to refresh"
        );

        let mut active_no_model =
            snapshot_with_panels(workspace, vec![provider_panel("codex", Vec::new())]);
        active_no_model
            .agents
            .push(root_agent("codex", "sess-active", None));
        assert_eq!(
            codex_session_refreshes(&active_no_model),
            vec![CodexSessionRefresh {
                session_id: "sess-active".to_owned(),
                model_hint: None,
            }],
            "a live Codex sidecar refreshes even with no model hint"
        );
    }

    /// The per-session throttle marker gates the app-server refresh: the first call
    /// is due (and touches the marker), the immediate next is not.
    #[test]
    fn codex_session_probe_throttles_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        assert!(codex_session_probe_due(&runtime, "sess/one"));
        assert!(
            !codex_session_probe_due(&runtime, "sess/one"),
            "a freshly-stamped session backs off"
        );
        assert!(
            codex_session_probe_due(&runtime, "sess/two"),
            "a different session has its own marker"
        );

        let old = SystemTime::now()
            .checked_sub(CODEX_RATE_LIMIT_REFRESH_INTERVAL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(codex_session_probe_marker(&runtime, "sess/one"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(
            codex_session_probe_due(&runtime, "sess/one"),
            "the session becomes due again after the 60s interval"
        );
    }

    #[test]
    fn reap_removes_stale_codex_probe_markers() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();

        let stale_codex = runtime.shared_root.join(format!(
            "{CODEX_PROBE_MARKER_PREFIX}00000000000000000000000000000000"
        ));
        let fresh_codex = runtime.shared_root.join(format!(
            "{CODEX_PROBE_MARKER_PREFIX}11111111111111111111111111111111"
        ));
        let accounts = runtime.shared_root.join("accounts.json");
        for path in [&stale_codex, &fresh_codex, &accounts] {
            std::fs::write(path, b"").unwrap();
        }
        let old = SystemTime::now()
            .checked_sub(CODEX_PROBE_MARKER_TTL + Duration::from_secs(1))
            .unwrap();
        std::fs::File::open(&stale_codex)
            .unwrap()
            .set_modified(old)
            .unwrap();

        reap_stale_codex_probe_markers(&runtime);

        assert!(!stale_codex.exists());
        assert!(fresh_codex.exists());
        assert!(accounts.exists());
    }

    #[test]
    fn codex_transcript_backstop_is_stat_gated() {
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

        refresh_codex_transcript_context(&runtime, "sess-1", Some("gpt-5"));
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

        refresh_codex_transcript_context(&runtime, "sess-1", Some("gpt-5"));
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
        refresh_codex_transcript_context(&runtime, "sess-1", Some("gpt-5"));
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
