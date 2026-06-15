use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::codex;
use crate::sidebar::timing::CODEX_RATE_LIMIT_REFRESH_INTERVAL;

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
        .join(format!("rate-limit-probe.codex.{}", &digest[..32]))
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
    let mut cmd = std::process::Command::new(exe);
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
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        session = %session_id,
        "sidebar: spawning codex context refresh",
    );
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "codex-refresh-context")
    {
        // Best-effort enrichment on a per-frame path: a missing `codex` binary
        // makes this fail (ENOENT) on every due frame, so a warn! here floods
        // the off-box channel with an environment fact that is not a Rimz fault.
        // Keep it at debug! for local diagnosis; it never reaches Sentry.
        tracing::debug!(
            session = %session_id,
            workspace = %runtime.workspace_id,
            tags.operation = "codex.context_refresh.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn codex context refresh",
        );
    }
}
