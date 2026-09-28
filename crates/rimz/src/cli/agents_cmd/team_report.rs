//! The `TEAM_REPORT` an agent-launched team owes its launcher at `Done`.
//!
//! The leader's team run collects its final message across turns. The flip
//! into `Done` publishes that message, queues one report to the launcher, and
//! settles the run; a flip back out opens a fresh run, so every `Done` reports.

use rimz::config::DONE_STAGE;
use rimz::disk::summary::FileSummary;
use rimz::harness::run;
use rimz::ids::MessageId;
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::sandbox::TmpView;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::store::run::{RunRecord, RunStatus};
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimeScope, Store};

use super::subagent_report::{ReportErr, ResponseFile, compose_digest_row};

/// What a stage flip did for the team's launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) enum TeamReportOutcome {
    /// A `TEAM_REPORT` is queued for `launcher`, and `delivered` when it went out now.
    Queued { launcher: String, delivered: bool },
    /// The board left `Done`; the leader's next stretch of work reports at the next `Done`.
    Reopened,
    /// No agent launched this cohort, or the flip does not touch `Done`.
    NotOwed,
}

/// Settle the cohort's team report for one stage flip of `instance` (`<team>#<channel>`).
pub(in crate::cli) fn on_flip(
    workspace: &ResolvedWorkspace,
    store: &Store,
    instance: &str,
    from: Option<&str>,
    to: &str,
) -> anyhow::Result<TeamReportOutcome> {
    let was_done = from == Some(DONE_STAGE);
    if to == DONE_STAGE && !was_done {
        return Ok(report_done(workspace, store, instance)?);
    }
    if was_done && to != DONE_STAGE && run::reopen_team_run(store.paths(), instance)?.is_some() {
        return Ok(TeamReportOutcome::Reopened);
    }
    Ok(TeamReportOutcome::NotOwed)
}

fn report_done(
    workspace: &ResolvedWorkspace,
    store: &Store,
    instance: &str,
) -> Result<TeamReportOutcome, ReportErr> {
    let Some(record) = run::open_team_run_for(store.paths(), instance)? else {
        return Ok(TeamReportOutcome::NotOwed);
    };
    // Constructed with a team marker by `open_team_run_for`'s filter.
    let Some(team) = record.team.as_ref() else {
        return Ok(TeamReportOutcome::NotOwed);
    };
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let agents = &projection.agents;
    let leader = rimz::address::launch_row(agents, &record.kind, &team.launch_id);
    let launcher = leader
        .and_then(|leader| leader.launcher())
        .and_then(|(kind, id)| rimz::address::launch_row(agents, kind, id))
        .filter(|launcher| launcher.ended_at.is_none());
    let (Some(leader), Some(launcher)) = (leader, launcher) else {
        // Nobody to tell: settle the run so the next Done does not report it.
        run::complete_team_run(store.paths(), &record.run_id, None)?;
        return Ok(TeamReportOutcome::NotOwed);
    };

    let published = run::publish_response(store.paths(), &record)?;
    let view = TmpView::current(
        Some(launcher.runs_in(crate::cli::machine_config().agents.isolation)),
        launcher.name.as_deref(),
        store.paths(),
    );
    let response = match published {
        Some(path) => Some(ResponseFile {
            summary: FileSummary::measure(&path).map_err(|source| ReportErr::Response {
                path: path.clone(),
                source,
            })?,
            path: view.agent_path(&path),
        }),
        None => None,
    };
    let settled = RunRecord {
        status: RunStatus::Completed,
        completed_at: Some(jiff::Timestamp::now()),
        ..record.clone()
    };
    let text = format!(
        "Team {instance} reached Done; its leader reports:\n{}",
        compose_digest_row(leader, &settled, response.as_ref())
    );
    let pane_id = launcher.pane.as_ref().map(|pane| &pane.pane_id);
    let mut message = MessageRecord::new(
        workspace.workspace_id.clone(),
        launcher,
        text,
        DeliveryGate::Done,
    )
    .with_channel(launcher.channel())
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::TeamReport,
    });
    if let Some(pane_id) = pane_id {
        message = message.with_pane_id(pane_id.clone());
    }
    let message_id: MessageId = message.message_id.clone();
    // Queue before settling: a crash in between re-reports at the next Done
    // rather than losing this one.
    store.queue_message(&message, &workspace.session_name)?;
    if run::complete_team_run(store.paths(), &record.run_id, Some(&message_id))?.is_none() {
        store.cancel_message(
            &message_id,
            &workspace.session_name,
            "team run already settled",
        )?;
        return Ok(TeamReportOutcome::NotOwed);
    }
    let delivered = match pane_id {
        Some(pane_id) => deliver_one(
            workspace,
            store,
            &message_id,
            Some(pane_id.mux()),
            DeliveryPolicy::Boundary,
        )?,
        None => false,
    };
    Ok(TeamReportOutcome::Queued {
        launcher: launcher
            .name
            .clone()
            .unwrap_or_else(|| launcher.agent_id.to_string()),
        delivered,
    })
}
