//! Supervised-run pane lookup and reclamation effects.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
#[cfg(test)]
use anyhow::bail;

#[cfg(test)]
use super::output;
use crate::cli::GlobalFlags;
use rimz::harness::run::RunRecord;
use rimz::ids::PaneId;
use rimz::mux::{
    PaneCmd, PaneListOptions, PaneReadConsistency, SplitPaneOptions, SplitPlacement, SplitTarget,
};
use rimz::room::session::MissingSessionReport;

pub(crate) const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const STOP_BACKSTOP_POLL: Duration = Duration::from_millis(250);

/// Split a run pane into the loop zone, repairing a missing loop panel first.
/// `Ok(false)` means the caller should fall back to a run tab.
pub(crate) fn split_into_loop_zone(
    backend: &dyn rimz::mux::MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
    cwd: &Path,
    env: BTreeMap<String, String>,
    pane: &PaneCmd,
) -> Result<bool> {
    let listing = match list_loop_zone_panes(backend, workspace) {
        Some(listing) => listing,
        None => return Ok(false),
    };
    let panel = match rimz::daemon_view::find_loop_panel(&listing.panes) {
        Some(panel) => panel.clone(),
        None => {
            let machine = rimz::config::MachineConfig::load_lenient();
            let rimz_bin = rimz::proc::rimz_exe();
            let claude_host_argv = (machine.remote_control.enabled_for("claude")
                && which::which("claude").is_ok())
            .then(|| rimz::agents::runtime_control::host_argv("claude"))
            .flatten();
            let view =
                rimz::daemon_view::daemon_view_spec(rimz::daemon_view::DaemonViewSpecParams {
                    claude_host_argv: claude_host_argv.as_deref(),
                    daemon: &machine.daemon,
                    rimz_bin: &rimz_bin,
                    workspace_id: &workspace.workspace_id,
                    session_name: &workspace.session_name,
                    project_root: &workspace.project_root,
                    worktree_root: &workspace.worktree_root,
                    codex_present: which::which("codex").is_ok(),
                });
            match rimz::daemon_view::ensure_loop_panel(
                backend,
                &workspace.session_name,
                &workspace.workspace_id,
                &view,
            ) {
                Some(panel) => panel,
                None => return Ok(false),
            }
        }
    };
    match backend.split_pane(SplitPaneOptions {
        target: SplitTarget::SessionPane {
            session_name: workspace.session_name.clone(),
            pane_id: panel.pane_id.clone(),
        },
        cwd: Some(cwd.to_string_lossy().into_owned()),
        command: Some(pane.argv.clone()),
        title: None,
        env,
        placement: SplitPlacement::Stacked,
        focus: false,
    }) {
        Ok(()) => Ok(true),
        Err(err) => {
            tracing::debug!(
                session = %workspace.session_name,
                pane = %panel.pane_id,
                error = &err as &dyn std::error::Error,
                "loop zone split failed; falling back to a run tab",
            );
            Ok(false)
        }
    }
}

fn list_loop_zone_panes(
    backend: &dyn rimz::mux::MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
) -> Option<rimz::mux::PaneListing> {
    match backend.list_panes(PaneListOptions {
        session_name: Some(workspace.session_name.clone()),
        workspace_id: Some(workspace.workspace_id.clone()),
        command_timeout: Some(Duration::from_millis(500)),
        consistency: PaneReadConsistency::PreferAuthoritative,
        ..Default::default()
    }) {
        Ok(listing) => Some(listing),
        Err(err) => {
            tracing::debug!(
                session = %workspace.session_name,
                error = &err as &dyn std::error::Error,
                "loop zone lookup failed; falling back to a run tab",
            );
            None
        }
    }
}

pub(crate) fn backend_for_workspace_session(
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
) -> Result<Box<dyn rimz::mux::MuxBackend>> {
    let mux =
        crate::cli::render::room::present_mux_pick(rimz::room::session::pick_mux_for_session(
            &workspace.session_name,
            globals.mux,
            MissingSessionReport::Silent,
        ))?;
    Ok(rimz::mux::backend_for(mux))
}

pub(crate) fn close_run_pane(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) {
    if let Some(pane_id) = record.pane_id.as_ref() {
        match backend.close_pane(session_name, pane_id) {
            Ok(()) => return,
            Err(err) => tracing::debug!(
                run_id = %record.run_id,
                pane = %pane_id,
                error = %err,
                "run cleanup could not close the recorded pane",
            ),
        }
    }
    let Some(pane) = resolve_run_pane_from_snapshot(store, session_name, record) else {
        return;
    };
    if let Err(err) = backend.close_pane(&pane.session_name, &pane.pane_id) {
        tracing::debug!(
            run_id = %record.run_id,
            pane = %pane.pane_id,
            error = %err,
            "run cleanup could not close the agent pane",
        );
    }
}

pub(crate) fn capture_failure_tail(
    backend: &dyn rimz::mux::MuxBackend,
    pane_id: &PaneId,
) -> Option<String> {
    // rimz-invariant: run-failure-capture
    let capture = match backend.capture_pane(pane_id, None, false) {
        Ok(capture) => capture,
        Err(err) => {
            tracing::debug!(
                pane = %pane_id,
                error = %err,
                "run failure pane capture unavailable",
            );
            return None;
        }
    };
    let tail = capture.raw_text.trim_end();
    if tail.trim().is_empty() {
        None
    } else {
        Some(tail.to_owned())
    }
}

pub(crate) fn close_stopped_run_pane_after_grace(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
    grace: Duration,
) {
    let deadline = Instant::now() + grace;
    loop {
        let Some((latest, pane)) = latest_resolved_run_pane(store, session_name, record) else {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(STOP_BACKSTOP_POLL);
            continue;
        };
        match backend.list_panes(PaneListOptions {
            session_name: Some(pane.session_name.clone()),
            command_timeout: Some(STOP_BACKSTOP_POLL),
            ..Default::default()
        }) {
            Ok(listing)
                if listing
                    .panes
                    .iter()
                    .any(|candidate| candidate.pane_id == pane.pane_id) =>
            {
                if Instant::now() >= deadline {
                    close_run_pane(backend, store, session_name, &latest);
                    return;
                }
            }
            Ok(_) => return,
            Err(err) => {
                tracing::debug!(
                    run_id = %record.run_id,
                    error = %err,
                    "run stop backstop skipped; pane list unavailable",
                );
                return;
            }
        }
        std::thread::sleep(STOP_BACKSTOP_POLL);
    }
}

pub(crate) fn latest_resolved_run_pane(
    store: &rimz::Store,
    session_name: &str,
    fallback: &RunRecord,
) -> Option<(RunRecord, ResolvedRunPane)> {
    let latest = latest_run_record(store, fallback);
    let pane = resolve_run_pane(store, session_name, &latest)?;
    Some((latest, pane))
}

fn latest_run_record(store: &rimz::Store, fallback: &RunRecord) -> RunRecord {
    rimz::harness::run::load(store.paths(), &fallback.run_id).unwrap_or_else(|err| {
        tracing::debug!(
            run_id = %fallback.run_id,
            error = %err,
            "run stop backstop using stale record; latest record unavailable",
        );
        fallback.clone()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedRunPane {
    pub(crate) pane_id: PaneId,
    pub(crate) session_name: String,
}

pub(crate) fn resolve_run_pane(
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    record
        .pane_id
        .as_ref()
        .map(|pane_id| ResolvedRunPane {
            pane_id: pane_id.clone(),
            session_name: session_name.to_owned(),
        })
        .or_else(|| resolve_run_pane_from_snapshot(store, session_name, record))
}

fn resolve_run_pane_from_snapshot(
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let snapshot = match store.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::debug!(run_id = %record.run_id, error = %err, "run pane resolution skipped; snapshot unavailable");
            return None;
        }
    };
    resolve_run_pane_in_snapshot(&snapshot, session_name, record)
}

pub(crate) fn resolve_run_pane_in_snapshot(
    snapshot: &rimz::SidebarSnapshot,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let agent_id = record.agent_id.as_ref()?;
    let pane = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == record.kind && agent.agent_id == *agent_id)
        .and_then(|agent| agent.pane.as_ref())?;
    Some(ResolvedRunPane {
        pane_id: pane.pane_id.clone(),
        session_name: if pane.session_name.is_empty() {
            session_name.to_owned()
        } else {
            pane.session_name.clone()
        },
    })
}

#[cfg(test)]
pub(crate) fn ensure_sendable(record: &RunRecord) -> Result<()> {
    if record.status.is_terminal() {
        bail!(
            "run {} is {}; nothing to send",
            record.run_id,
            output::status_label(record.status)
        );
    }
    Ok(())
}
