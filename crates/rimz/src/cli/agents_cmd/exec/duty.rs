//! Store-owning duties delegated by the parked exec image.

use super::*;
use rimz::harness::parent_watch::{ProbeConfirm, WatchdogSeed};
use rimz::ids::{PaneId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::process::Stdio;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(in crate::cli::agents_cmd) struct SuperviseDutyRequest {
    workspace_id: WorkspaceId,
    session_name: String,
    wrapper_pid: u32,
    duty: Duty,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum Duty {
    Strand {
        run_id: rimz::RunId,
    },
    Receipt {
        run_id: rimz::RunId,
        report: bool,
    },
    ParentProbe {
        child_kind: AgentKind,
        child_launch_id: AgentSessionId,
        child_pane: Option<PaneId>,
        pane_strikes: bool,
    },
    CardEvidence {
        kind: AgentKind,
        launch_id: AgentSessionId,
        name: String,
    },
}

pub(super) fn command(
    workspace_id: WorkspaceId,
    session_name: String,
    duty: Duty,
) -> Result<Command> {
    let request = SuperviseDutyRequest {
        workspace_id,
        session_name,
        duty,
        wrapper_pid: std::process::id(),
    };
    let mut command = Command::new(rimz::proc::rimz_exe());
    command
        .args(["agents", "supervise-duty", "--request"])
        .arg(serde_json::to_string(&request)?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

pub(super) fn confirm(
    workspace_id: &WorkspaceId,
    seed: &WatchdogSeed,
    pane_strikes: bool,
) -> ProbeConfirm {
    let output = command(
        workspace_id.clone(),
        seed.session_name.clone(),
        Duty::ParentProbe {
            child_kind: seed.child_kind.clone(),
            child_launch_id: seed.child_launch_id.clone(),
            child_pane: seed.child_pane.clone(),
            pane_strikes,
        },
    )
    .and_then(|mut command| {
        command.stdout(Stdio::piped());
        command.output().context("running parent confirmation duty")
    });
    match output {
        Ok(output) if output.status.code() == Some(0) => ProbeConfirm::Ended,
        Ok(output) if output.status.code() == Some(3) => {
            match serde_json::from_slice::<WatchdogSeed>(&output.stdout) {
                Ok(answer) => ProbeConfirm::Alive(Box::new(answer)),
                Err(error) => {
                    tracing::debug!(%error, "invalid parent confirmation answer");
                    ProbeConfirm::Unknown
                }
            }
        }
        Ok(output) => {
            tracing::debug!(status = ?output.status, "parent confirmation duty failed");
            ProbeConfirm::Unknown
        }
        Err(error) => {
            tracing::debug!(%error, "parent confirmation duty failed");
            ProbeConfirm::Unknown
        }
    }
}

pub(in crate::cli::agents_cmd) fn run(request: SuperviseDutyRequest) -> Result<()> {
    #[cfg(unix)]
    rimz::child_process::CleanupSignalMask::unblock()?;
    let ctx = Ctx::for_workspace(request.workspace_id, None)?;
    let context = |run_id| RunExecContext {
        run_id,
        store: ctx.store.clone(),
        session_name: request.session_name.clone(),
        workspace: ctx.workspace.clone(),
    };
    let yes = match request.duty {
        Duty::Strand { run_id } => {
            let context = context(run_id);
            let mut previous = None;
            let wrapper_is_parent = || {
                #[cfg(unix)]
                {
                    std::os::unix::process::parent_id() == request.wrapper_pid
                }
                #[cfg(not(unix))]
                {
                    true
                }
            };
            while wrapper_is_parent() && !run_strand_once(&context, &mut previous)? {
                std::thread::sleep(PARK_STRAND_POLL);
            }
            true
        }
        Duty::Receipt { run_id, report } => run_receipt_once(&context(run_id), report)?,
        Duty::CardEvidence {
            kind,
            launch_id,
            name,
        } => run_card_evidence(&ctx.store, &kind, &launch_id, &name)?,
        Duty::ParentProbe {
            child_kind,
            child_launch_id,
            child_pane,
            pane_strikes,
        } => {
            let (ended, answer) = run_parent_probe(
                &ctx.store,
                child_kind,
                child_launch_id,
                child_pane,
                request.session_name,
                pane_strikes,
            )?;
            serde_json::to_writer(std::io::stdout().lock(), &answer)?;
            writeln!(std::io::stdout().lock())?;
            ended
        }
    };
    std::process::exit(if yes { 0 } else { 3 });
}

fn run_strand_once(
    context: &RunExecContext,
    previous: &mut Option<jiff::Timestamp>,
) -> Result<bool> {
    let record = rimz::harness::run::load(context.store.paths(), &context.run_id)?;
    if record.status.is_terminal() || record.parked_at.is_none() {
        return Ok(true);
    }
    repair_digest(context, &record);
    *previous = match rimz::harness::run::settle_stranded_park(&context.store, &record, *previous)?
    {
        rimz::harness::run::ParkCheck::Stranded(at) => Some(at),
        _ => None,
    };
    Ok(false)
}

fn run_receipt_once(context: &RunExecContext, report: bool) -> Result<bool> {
    let record = rimz::harness::run::load(context.store.paths(), &context.run_id)?;
    if !record.status.is_terminal() {
        return Ok(false);
    }
    if report && record.owes_report() {
        super::super::subagent_report::report_settled_child(
            &context.workspace,
            &context.store,
            &record,
        )?;
    }
    let record = rimz::harness::run::load(context.store.paths(), &context.run_id)?;
    Ok(context.parent_received_and_rested(&record)?)
}

/// The strand settle exits a park that is owed nothing; a settled fleet
/// whose digest was lost is owed something no one else will deliver, since
/// the child's reporter does not retry and `orphan_sweep`'s backstop needs
/// a live sidebar producer. So the parked run repairs its own fleet before
/// each strand check. The repair is idempotent and its stamp CAS tolerates
/// a concurrent reporter, so every outcome and error is logged and ignored:
/// it never gates the check that follows it. The same repair settles ended
/// team cohorts before the fleet reporter runs. A park that launched nobody
/// costs one projection read per reporter, since each answers a launcher
/// with no members or team seats without listing the runs.
fn repair_digest(context: &RunExecContext, record: &rimz::store::run::RunRecord) {
    let Some(agent_id) = record.agent_id.as_ref() else {
        return;
    };
    if let Err(error) =
        super::super::team_report::settle_ended_teams(&context.workspace, &context.store, agent_id)
    {
        tracing::debug!(run_id = %context.run_id, %error, "could not settle a parked run's ended teams");
    }
    match super::super::subagent_report::report_fleet(&context.workspace, &context.store, agent_id)
    {
        Ok(outcome) => {
            tracing::debug!(run_id = %context.run_id, ?outcome, "repaired a parked run's fleet digest");
        }
        Err(error) => {
            tracing::debug!(run_id = %context.run_id, %error, "could not repair a parked run's fleet digest");
        }
    }
}

pub(super) fn parent_cursor(paths: &rimz::StatePaths) -> Result<rimz::store::event_log::LogExtent> {
    let _guard = rimz::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    loop {
        let generation = rimz::store::snapshot::lifecycle_log_generation(paths);
        let offset = std::fs::metadata(&paths.events_log).map_or(0, |meta| meta.len());
        if generation == rimz::store::snapshot::lifecycle_log_generation(paths) {
            return Ok(rimz::store::event_log::LogExtent { generation, offset });
        }
    }
}

fn run_parent_probe(
    store: &rimz::Store,
    child_kind: AgentKind,
    child_launch_id: AgentSessionId,
    child_pane: Option<PaneId>,
    session_name: String,
    pane_strikes: bool,
) -> Result<(bool, WatchdogSeed)> {
    let cursor = parent_cursor(store.paths())?;
    let projection = store.runtime_projection(rimz::store::runtime::RuntimeScope::Audit)?;
    let seed = rimz::harness::parent_watch::seed(
        &projection.agents,
        child_kind,
        child_launch_id,
        child_pane,
        session_name,
        cursor,
    )
    .context("parent launch is not in the runtime projection")?;
    let mut ended = !seed.members.is_empty() && seed.members.values().all(|ended| *ended);
    if pane_strikes
        && !ended
        && let Some(parent_pane) = &seed.parent_pane
    {
        let backend = rimz::mux::backend_for(parent_pane.mux());
        let cached_present = backend
            .cached_pane_roster(&seed.session_name, &store.paths().workspace_id)
            .is_some_and(|roster| roster.pane_ids.contains(parent_pane));
        if !cached_present {
            let listing = backend.list_panes(rimz::mux::PaneListOptions {
                session_name: Some(seed.session_name.clone()),
                workspace_id: Some(store.paths().workspace_id.clone()),
                consistency: rimz::mux::PaneReadConsistency::RequireAuthoritative,
                command_timeout: Some(Duration::from_secs(5)),
                ..rimz::mux::PaneListOptions::default()
            })?;
            ended = !listing
                .panes
                .iter()
                .any(|pane| pane.pane_id == *parent_pane);
        }
    }
    Ok((ended, seed))
}

pub(super) fn run_card_evidence(
    store: &rimz::Store,
    kind: &AgentKind,
    launch_id: &AgentSessionId,
    name: &str,
) -> Result<bool> {
    Ok(store.snapshot_cached()?.agents.iter().any(|agent| {
        agent.kind == *kind && agent.agent_id == *launch_id && agent.name.as_deref() == Some(name)
    }))
}

#[cfg(test)]
mod tests;
