//! Supervised-run pane lookup and reclamation effects.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::cli::GlobalFlags;
use rimz::ids::PaneId;
use rimz::mux::{PaneListOptions, PaneReadConsistency};
use rimz::store::run::RunRecord;
use rimz::store::snapshot::find_agent;

pub(super) const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const STOP_BACKSTOP_POLL: Duration = Duration::from_millis(250);
pub(super) fn backend_for_workspace_session(
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
) -> Result<Box<dyn rimz::mux::MuxBackend>> {
    let mux = crate::cli::render::room::present_mux_pick(
        rimz::room::session::pick_mux_for_session(&workspace.session_name, globals.mux),
    )?;
    Ok(rimz::mux::backend_for(mux))
}

/// Close the recorded pane, then the agent row's; `Ok` when either close went
/// through or the run has no pane.
pub(super) fn close_run_pane(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) -> rimz::mux::Result<()> {
    if run_predates_rebirth(store, record).map_err(std::io::Error::other)? {
        return Ok(());
    }
    let mut recorded = Ok(());
    if let Some(pane_id) = record.pane_id.as_ref() {
        rimz::harness::hibernate::wake_pane(store, pane_id).map_err(std::io::Error::other)?;
        match backend.close_pane(session_name, pane_id) {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::debug!(
                    run_id = %record.run_id,
                    pane = %pane_id,
                    error = %err,
                    "run cleanup could not close the recorded pane",
                );
                recorded = Err(err);
            }
        }
    }
    let Some(pane) = resolve_run_pane_from_snapshot(store, session_name, record) else {
        return recorded;
    };
    rimz::harness::hibernate::wake_pane(store, &pane.pane_id).map_err(std::io::Error::other)?;
    backend.close_pane(&pane.session_name, &pane.pane_id)
}

pub(crate) fn capture_failure_tail(
    backend: &dyn rimz::mux::MuxBackend,
    pane_id: &PaneId,
    session_name: &str,
) -> Option<String> {
    // rimz-invariant: run-failure-capture
    let capture = match backend.capture_pane(pane_id, session_name, Some(60), false) {
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

/// A stopped run's pane that was not confirmed closed. The caller names the
/// stop that retries the close.
#[derive(Debug)]
pub(crate) struct PaneOpen {
    pub(super) pane: Option<PaneId>,
    pub(super) reason: String,
}

impl std::fmt::Display for PaneOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.pane {
            Some(pane) => write!(f, "pane {pane} is still open")?,
            None => f.write_str("the run's pane was not closed")?,
        }
        write!(f, ": {}", self.reason)
    }
}

pub(super) fn close_stopped_run_pane_after_grace(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
    grace: Duration,
) -> Result<(), PaneOpen> {
    settle_stopped_run_pane(
        store,
        session_name,
        record,
        grace,
        |pane| {
            backend
                .list_panes(PaneListOptions {
                    session_name: Some(pane.session_name.clone()),
                    command_timeout: Some(STOP_BACKSTOP_POLL),
                    // A cached topology may predate the pane: only mux truth proves it closed.
                    consistency: PaneReadConsistency::RequireAuthoritative,
                    ..Default::default()
                })
                .map(|listing| {
                    listing
                        .panes
                        .iter()
                        .any(|candidate| candidate.pane_id == pane.pane_id)
                })
                .inspect_err(|err| {
                    tracing::debug!(
                        run_id = %record.run_id,
                        error = %err,
                        "run stop backstop: pane list unavailable",
                    );
                })
        },
        |latest| close_run_pane(backend, store, session_name, latest),
    )
}

/// The backstop's verdict. `listed` answers whether the pane is still in the
/// multiplexer's listing. A listing without the pane, a session that is gone,
/// or a close that went through proves it closed; any other listing error
/// proves nothing. A run whose pane rimz cannot name has nothing to close.
pub(super) fn settle_stopped_run_pane(
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
    grace: Duration,
    mut listed: impl FnMut(&ResolvedRunPane) -> rimz::mux::Result<bool>,
    close: impl FnOnce(&RunRecord) -> rimz::mux::Result<()>,
) -> Result<(), PaneOpen> {
    let deadline = Instant::now() + grace;
    loop {
        let resolved = latest_resolved_run_pane(store, session_name, record);
        if let Some((_, pane)) = &resolved
            && pane_is_gone(&listed(pane))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let Some((latest, pane)) = resolved else {
                return Ok(());
            };
            let Err(err) = close(&latest) else {
                return Ok(());
            };
            if pane_is_gone(&listed(&pane)) {
                return Ok(());
            }
            return Err(PaneOpen {
                pane: Some(pane.pane_id),
                reason: err.to_string(),
            });
        }
        std::thread::sleep(STOP_BACKSTOP_POLL);
    }
}

fn pane_is_gone(listed: &rimz::mux::Result<bool>) -> bool {
    matches!(
        listed,
        Ok(false) | Err(rimz::mux::MuxErr::SessionNotFound { .. })
    )
}

pub(super) fn latest_resolved_run_pane(
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
pub(super) struct ResolvedRunPane {
    pub(super) pane_id: PaneId,
    pub(super) session_name: String,
}

pub(super) fn resolve_run_pane(
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    if run_predates_rebirth(store, record).ok()? {
        return None;
    }
    record
        .pane_id
        .as_ref()
        .map(|pane_id| ResolvedRunPane {
            pane_id: pane_id.clone(),
            session_name: session_name.to_owned(),
        })
        .or_else(|| resolve_run_pane_from_snapshot(store, session_name, record))
}

fn run_predates_rebirth(store: &rimz::Store, record: &RunRecord) -> Result<bool> {
    // Only workflow attempts are canceled across a rebirth and stopped afterward by another process.
    if record.workflow.is_none() {
        return Ok(false);
    }
    match std::fs::read(&store.paths().last_death_marker) {
        Ok(bytes) => {
            let death: rimz::store::event::LastDeathMarker = serde_json::from_slice(&bytes)?;
            if record.started_at <= death.at {
                return Ok(true);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(store.read_events()?.iter().any(|event| {
        event.timestamp >= record.started_at
            && matches!(event.kind(), rimz::store::event::EventKind::SessionRebirth)
    }))
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

pub(super) fn resolve_run_pane_in_snapshot(
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let agent_id = record.agent_id.as_ref()?;
    let pane = find_agent(&snapshot.agents, &record.kind, agent_id)
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
