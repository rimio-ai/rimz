//! Producer-owned live-session context refresh.
//!
//! The elected sidebar producer asks each live root session's adapter for cheap
//! transcript-tail refreshes and optional detached rich-context helpers through
//! the same trigger seams hooks use.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::{
    AgentState, AgentTurnError, LifecycleRefreshCtx, LocalContextRefresh, LocalContextRefreshCtx,
    RefreshSpawn, RefreshTrigger,
};
use crate::ids::PaneId;
use crate::sidebar::timing::SESSION_REFRESH_INTERVAL;
use crate::store::gc::{SESSION_PROBE_MARKER_PREFIX, SESSION_PROBE_MARKER_TTL};

use super::SidebarSnapshot;

const CODEX_TURN_DEATH_CAPTURE_LINES: u16 = 60;
const CODEX_TURN_DEATH_RETRY_WINDOW: Duration = Duration::from_secs(10 * 60);

type SessionRefreshResult<T> = std::result::Result<T, crate::disk::atomic::AtomicErr>;

/// Refresh every live root session's adapter-owned context sidecar from the
/// producer. Inline transcript reads run first with their adapter stat gate;
/// detached helpers run on a coarse per-session cadence for richer realtime
/// channels.
pub(super) fn refresh_live_sessions(snapshot: &SidebarSnapshot, runtime: &RuntimePaths) {
    for refresh in live_session_refreshes(snapshot) {
        refresh_session_transcript_context_with_snapshot(
            Some(snapshot),
            runtime,
            &refresh.kind,
            &refresh.session_id,
            refresh.model_hint.as_deref(),
            RefreshTrigger::Tick,
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

/// Refresh one watched transcript without running adapter full-history work.
pub fn refresh_session_transcript_context_from_watch(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    model_hint: Option<&str>,
) {
    refresh_session_transcript_context_with_snapshot(
        None,
        runtime,
        kind,
        session_id,
        model_hint,
        RefreshTrigger::Watch,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForcedSessionRefresh {
    pub transcript_refreshed: bool,
    pub helper_spawned: bool,
}

/// Force one session's local context refresh for a user-requested card update.
/// The transcript stat gate is bypassed, while the remembered transcript path
/// stays in use so the adapter re-reads the known rollout tail before falling
/// back to discovery.
pub fn force_refresh_session_context(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    agent: &AgentState,
) -> SessionRefreshResult<ForcedSessionRefresh> {
    let kind = agent.kind.as_str();
    let session_id = agent.agent_id.as_str();
    let model_hint = session_model_hint(agent);
    let transcript_refreshed = refresh_session_transcript_context_core(
        Some(snapshot),
        runtime,
        kind,
        session_id,
        model_hint,
        true,
        RefreshTrigger::Tick,
    )?;
    let spawn = session_context_refresh_spawn(runtime, kind, session_id, model_hint);
    let helper_spawned = spawn.is_some();
    if let Some(spawn) = spawn {
        let _ = session_probe_due(runtime, kind, session_id);
        spawn_forced_session_context_refresh(runtime, kind, session_id, spawn);
    }
    Ok(ForcedSessionRefresh {
        transcript_refreshed,
        helper_spawned,
    })
}

fn spawn_forced_session_context_refresh(
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    spawn: RefreshSpawn,
) {
    // Unit tests exercise the inline merge without forking a detached helper.
    if cfg!(test) {
        return;
    }
    spawn_session_context_refresh(runtime, kind, session_id, spawn);
}

fn refresh_session_transcript_context_with_snapshot(
    snapshot: Option<&SidebarSnapshot>,
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    model_hint: Option<&str>,
    trigger: RefreshTrigger<'_>,
) {
    if let Err(err) = refresh_session_transcript_context_core(
        snapshot, runtime, kind, session_id, model_hint, false, trigger,
    ) {
        warn_session_transcript_merge(kind, session_id, &err);
    }
}

fn refresh_session_transcript_context_core(
    snapshot: Option<&SidebarSnapshot>,
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    model_hint: Option<&str>,
    force: bool,
    trigger: RefreshTrigger<'_>,
) -> SessionRefreshResult<bool> {
    let Some(adapter) = crate::agents::find_definition(kind) else {
        return Ok(false);
    };
    let prior = crate::store::agent_context::read_one(runtime, kind, session_id);
    let shared_pricing_cache_path = runtime.shared_pricing_cache_path();
    let login_env = crate::agents::ambient_env();
    let ctx = LocalContextRefreshCtx {
        login_env: &login_env,
        agent_id: session_id,
        model_hint,
        prior_session_name: prior
            .as_ref()
            .and_then(|record| record.context.session_name.as_deref()),
        current_transcript_path: None,
        prior_transcript_path: prior
            .as_ref()
            .and_then(|record| record.transcript_path.as_deref()),
        prior_transcript_stat: if force {
            None
        } else {
            prior
                .as_ref()
                .and_then(|record| record.transcript_stat.as_ref())
        },
        prior_spend_fold: prior.as_ref().and_then(|record| record.spend_fold.as_ref()),
        shared_pricing_cache_path: &shared_pricing_cache_path,
    };
    let refresh = adapter.local_context_refresh(trigger, &ctx);
    let Some(refresh) = refresh else {
        return retry_unconfirmed_codex_turn_death(
            snapshot,
            runtime,
            kind,
            session_id,
            prior.as_ref(),
        );
    };
    let mut refresh = refresh;
    if let Some(snapshot) = snapshot {
        confirm_codex_turn_death_from_snapshot(snapshot, runtime, kind, session_id, &mut refresh);
    }
    crate::store::agent_context::merge_local_context(
        runtime,
        adapter.spec(),
        session_id,
        refresh,
        Timestamp::now(),
    )?;
    let _ = crate::wakeup::wake_store_delta(runtime, None, None);
    Ok(true)
}

fn warn_session_transcript_merge(
    kind: &str,
    session_id: &str,
    err: &crate::disk::atomic::AtomicErr,
) {
    tracing::warn!(
        kind,
        session = %session_id,
        tags.operation = "session.transcript_merge",
        error = err as &dyn std::error::Error,
        "sidebar: failed to merge session transcript context",
    );
}

fn confirm_codex_turn_death_from_snapshot(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    refresh: &mut LocalContextRefresh,
) {
    let crate::agents::FieldPatch::Set(error) = &mut refresh.context.turn_error else {
        return;
    };
    if kind != "codex"
        || !crate::agents::session::turn_death_needs_pane_confirmation("codex", error)
    {
        return;
    }
    let pane = session_pane_from_snapshot(snapshot, kind, session_id);
    confirm_codex_turn_death_from_pane(runtime, pane, error);
}

fn session_pane_from_snapshot<'a>(
    snapshot: &'a SidebarSnapshot,
    kind: &str,
    session_id: &str,
) -> Option<&'a PaneId> {
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind.as_str() == kind
                && pane
                    .agent_id
                    .as_ref()
                    .is_some_and(|agent_id| agent_id.as_str() == session_id)
        })
        .map(|pane| &pane.pane_id)
}

pub fn confirm_codex_turn_death_from_pane(
    runtime: &RuntimePaths,
    pane: Option<&PaneId>,
    error: &mut AgentTurnError,
) {
    if !crate::agents::session::turn_death_needs_pane_confirmation("codex", error) {
        return;
    }
    if let Some(pane) = pane {
        let backend = crate::mux::backend_for(pane.mux());
        // rimz-invariant: codex-turn-death-confirmation
        if let Ok(capture) = backend.capture_pane(pane, Some(CODEX_TURN_DEATH_CAPTURE_LINES), false)
        {
            crate::agents::session::refine_turn_death_from_frame("codex", error, &capture.raw_text);
        }
    }
    if crate::agents::session::turn_death_needs_pane_confirmation("codex", error) {
        let now = Timestamp::now();
        let capacity = crate::agents::ProviderCapacity::read(runtime, "codex");
        crate::agents::session::infer_turn_death_from_spent_window(
            "codex",
            error,
            capacity.as_ref(),
            now,
        );
    }
}

fn retry_unconfirmed_codex_turn_death(
    snapshot: Option<&SidebarSnapshot>,
    runtime: &RuntimePaths,
    kind: &str,
    session_id: &str,
    prior: Option<&crate::agents::context::record::AgentContextRecord>,
) -> SessionRefreshResult<bool> {
    let Some(error) = prior.and_then(|record| record.context.turn_error.as_ref()) else {
        return Ok(false);
    };
    if !codex_turn_death_retry_due(kind, error, Timestamp::now()) {
        return Ok(false);
    }
    let mut marker = error.clone();
    let pane = snapshot.and_then(|snapshot| session_pane_from_snapshot(snapshot, kind, session_id));
    confirm_codex_turn_death_from_pane(runtime, pane, &mut marker);
    if marker == *error {
        return Ok(false);
    }
    let changed = crate::store::agent_context::merge_turn_error(runtime, kind, session_id, marker)?;
    if changed {
        let _ = crate::wakeup::wake_store_delta(runtime, None, None);
    }
    Ok(changed)
}

fn codex_turn_death_retry_due(kind: &str, error: &AgentTurnError, now: Timestamp) -> bool {
    // A marker stamped ahead of this clock is not recent; it becomes due once the clock passes it.
    let age = now.duration_since(error.at);
    kind == "codex"
        && crate::agents::session::turn_death_needs_pane_confirmation("codex", error)
        && !age.is_negative()
        && age.as_secs() <= CODEX_TURN_DEATH_RETRY_WINDOW.as_secs() as i64
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveSessionRefresh {
    pub kind: String,
    pub session_id: String,
    pub model_hint: Option<String>,
}

fn live_session_refreshes(snapshot: &SidebarSnapshot) -> Vec<LiveSessionRefresh> {
    snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| crate::agents::find_definition(agent.kind.as_str()).is_some())
        .map(|agent| LiveSessionRefresh {
            kind: agent.kind.as_str().to_owned(),
            session_id: agent.agent_id.to_string(),
            model_hint: session_model_hint(agent).map(str::to_owned),
        })
        .collect()
}

fn session_model_hint(agent: &AgentState) -> Option<&str> {
    agent.model.as_deref().or_else(|| {
        agent
            .context
            .as_ref()
            .and_then(|context| context.model_id.as_deref())
    })
}

/// Throttle one session's detached context refresh via a marker file under the
/// runtime root: skip when the last attempt is younger than the interval, touch
/// it before spawning.
fn session_probe_due(runtime: &RuntimePaths, kind: &str, session_id: &str) -> bool {
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

fn session_probe_marker(runtime: &RuntimePaths, kind: &str, session_id: &str) -> PathBuf {
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
    let adapter = crate::agents::find_definition(kind)?;
    let refresh_ctx = LifecycleRefreshCtx {
        agent_id: session_id,
        workspace_id: &runtime.workspace_id,
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
        // a RimZ fault. Keep it at debug! so it never reaches Sentry.
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
mod tests;
