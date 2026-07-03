use std::time::{Duration, Instant};

use anyhow::Result;
#[cfg(test)]
use anyhow::bail;

#[cfg(test)]
use super::output;
use crate::cli::GlobalFlags;
use crate::cli::room::{MissingSessionReport, pick_mux_for_session};
use rimz::harness::run::RunRecord;
use rimz::ids::PaneId;
use rimz::mux::PaneListOptions;

pub(crate) const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const STOP_BACKSTOP_POLL: Duration = Duration::from_millis(250);

pub(crate) fn backend_for_workspace_session(
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
) -> Result<Box<dyn rimz::mux::MuxBackend>> {
    let mux = pick_mux_for_session(
        &workspace.session_name,
        globals.mux,
        MissingSessionReport::Silent,
    )?;
    Ok(rimz::mux::backend_for(mux))
}

pub(crate) fn close_run_pane(
    backend: &dyn rimz::mux::MuxBackend,
    ledger: &rimz::Ledger,
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
    let Some(pane) = resolve_run_pane_from_snapshot(ledger, session_name, record) else {
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
    ledger: &rimz::Ledger,
    session_name: &str,
    record: &RunRecord,
    grace: Duration,
) {
    let deadline = Instant::now() + grace;
    loop {
        let Some((latest, pane)) = latest_resolved_run_pane(ledger, session_name, record) else {
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
                    close_run_pane(backend, ledger, session_name, &latest);
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
    ledger: &rimz::Ledger,
    session_name: &str,
    fallback: &RunRecord,
) -> Option<(RunRecord, ResolvedRunPane)> {
    let latest = latest_run_record(ledger, fallback);
    let pane = resolve_run_pane(ledger, session_name, &latest)?;
    Some((latest, pane))
}

fn latest_run_record(ledger: &rimz::Ledger, fallback: &RunRecord) -> RunRecord {
    rimz::harness::run::load(ledger.paths(), &fallback.run_id).unwrap_or_else(|err| {
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
    ledger: &rimz::Ledger,
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
        .or_else(|| resolve_run_pane_from_snapshot(ledger, session_name, record))
}

fn resolve_run_pane_from_snapshot(
    ledger: &rimz::Ledger,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let snapshot = match ledger.snapshot_cached() {
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
