use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::codex;
use crate::sidebar::timing::CODEX_RATE_LIMIT_REFRESH_INTERVAL;

use super::SidebarSnapshot;

/// Refresh Codex enrichment from the producer. A live/root Codex session first
/// refreshes its transcript-derived tokens/cost in process with a stat gate, then
/// the existing detached helper refreshes app-server-owned budget/account fields
/// on the coarse per-target cadence. A logged-in metered Codex account with no
/// root session refreshes the account cache instead, so idle dashboards stay
/// current.
pub(super) fn refresh_codex_rate_limits(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    for refresh in codex_rate_limit_refreshes(snapshot) {
        if let CodexRateLimitRefresh::Session {
            session_id,
            model_hint,
        } = &refresh
        {
            refresh_codex_transcript_context(runtime, session_id, model_hint.as_deref());
        }
        if !codex_rate_limit_probe_due(runtime, &refresh) {
            continue;
        }
        match refresh {
            CodexRateLimitRefresh::Session {
                session_id,
                model_hint,
            } => spawn_codex_context_refresh(runtime, &session_id, model_hint.as_deref()),
            CodexRateLimitRefresh::Account => spawn_codex_account_window_fetch(runtime),
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
pub(crate) enum CodexRateLimitRefresh {
    Session {
        session_id: String,
        model_hint: Option<String>,
    },
    Account,
}

pub(crate) fn codex_rate_limit_refreshes(snapshot: &SidebarSnapshot) -> Vec<CodexRateLimitRefresh> {
    let sessions = snapshot
        .agents
        .iter()
        .filter(|agent| agent.kind == "codex" && agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .map(|agent| CodexRateLimitRefresh::Session {
            session_id: agent.agent_id.to_string(),
            model_hint: agent
                .model
                .clone()
                .or_else(|| agent.context.as_ref().and_then(|ctx| ctx.model_id.clone())),
        })
        .collect::<Vec<_>>();
    if !sessions.is_empty() {
        return sessions;
    }

    snapshot
        .providers
        .iter()
        .filter(|panel| provider_has_out_of_band_windows(&panel.kind) && panel.metered)
        .map(|_| CodexRateLimitRefresh::Account)
        .collect()
}

/// Whether a provider kind exposes an account-scoped, sessionless rate-limit read
/// the producer can fetch out-of-band. Codex serves it from its app-server;
/// Claude has none (its windows ride a live statusline), so it never qualifies.
pub(crate) fn provider_has_out_of_band_windows(kind: &str) -> bool {
    kind == "codex"
}

/// Throttle one Codex rate-limit refresh target via a marker file under the
/// runtime root: skip when the last attempt is younger than the interval, touch
/// it before spawning. Windows move on the scale of minutes, so a one-minute
/// gate keeps a slow/unreachable app-server from spawning a helper every frame
/// while still updating during long-running turns.
pub(crate) fn codex_rate_limit_probe_due(
    runtime: &RuntimePaths,
    refresh: &CodexRateLimitRefresh,
) -> bool {
    let path = codex_rate_limit_probe_marker(runtime, refresh);
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

pub(crate) fn codex_rate_limit_probe_marker(
    runtime: &RuntimePaths,
    refresh: &CodexRateLimitRefresh,
) -> PathBuf {
    match refresh {
        CodexRateLimitRefresh::Account => runtime.shared_root.join("rate-limit-probe.codex"),
        CodexRateLimitRefresh::Session { session_id, .. } => {
            let mut hasher = Sha256::new();
            hasher.update(b"codex-session");
            hasher.update([0]);
            hasher.update(session_id.as_bytes());
            let digest = hex::encode(hasher.finalize());
            runtime
                .shared_root
                .join(format!("rate-limit-probe.codex.{}", &digest[..32]))
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
        tracing::warn!(
            session = %session_id,
            workspace = %runtime.workspace_id,
            tags.operation = "codex.context_refresh.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn codex context refresh",
        );
    }
}

/// Spawn the detached, fresh-stdio helper that fetches Codex's account windows
/// and merges them into the shared cache. Best-effort: a spawn failure is logged
/// and dropped — the dashboard keeps the prior reading until the next due frame.
fn spawn_codex_account_window_fetch(runtime: &RuntimePaths) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(
                workspace = %runtime.workspace_id,
                tags.operation = "codex.rate_limits.locate_exe",
                error = &err as &dyn std::error::Error,
                "sidebar: cannot locate rimz to refresh codex windows",
            );
            return;
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "codex",
        "refresh-rate-limits",
        "--workspace-id",
        runtime.workspace_id.as_str(),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        "sidebar: spawning codex rate-limit refresh",
    );
    if let Err(err) =
        crate::child_process::spawn_detached_reaped(&mut cmd, "codex-refresh-rate-limits")
    {
        tracing::warn!(
            workspace = %runtime.workspace_id,
            tags.operation = "codex.rate_limits.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn codex rate-limit refresh",
        );
    }
}
