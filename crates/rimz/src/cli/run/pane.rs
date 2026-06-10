use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::super::{GlobalFlags, MissingSessionReport, pick_mux_for_session};
use super::output;
use rimz::ids::PaneId;
use rimz::mux::PaneListOptions;
use rimz::run::RunRecord;

pub(super) const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const STOP_BACKSTOP_POLL: Duration = Duration::from_millis(250);

pub(super) fn backend_for_workspace_session(
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

pub(super) fn close_run_pane(
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

pub(super) fn close_stopped_run_pane_after_grace(
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
            Ok(panes)
                if panes
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

pub(super) fn latest_resolved_run_pane(
    ledger: &rimz::Ledger,
    session_name: &str,
    fallback: &RunRecord,
) -> Option<(RunRecord, ResolvedRunPane)> {
    let latest = latest_run_record(ledger, fallback);
    let pane = resolve_run_pane(ledger, session_name, &latest)?;
    Some((latest, pane))
}

fn latest_run_record(ledger: &rimz::Ledger, fallback: &RunRecord) -> RunRecord {
    rimz::run::load(ledger.paths(), &fallback.run_id).unwrap_or_else(|err| {
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

pub(super) fn resolve_run_pane_in_snapshot(
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

pub(super) fn ensure_sendable(record: &RunRecord) -> Result<()> {
    if record.status.is_terminal() {
        bail!(
            "run {} is {}; nothing to send",
            record.run_id,
            output::status_label(record.status)
        );
    }
    Ok(())
}
