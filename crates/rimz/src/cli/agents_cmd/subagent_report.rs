//! Durable parent-facing completion digests for launched subagent fleets.
//!
//! Response files land per child; run records are stamped per fleet before
//! the digest enters the message queue. This makes the complete row set
//! visible to inline join cancellation and lets the wrapper fast path race
//! safely with the producer backstop.

use std::collections::HashMap;

use anyhow::Context;
use jiff::Timestamp;
use rimz::agents::AgentState;
use rimz::disk::summary::FileSummary;
use rimz::harness::fleet::FleetRuns;
use rimz::harness::run;
use rimz::harness::run::report::{Clock, ReportErr, ResponseFile, ResumeAccount, compose_digest};
use rimz::ids::{AgentKind, AgentSessionId, MessageId, RunId};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::store::run::RunRecord;
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimeScope, Store};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReportOutcome {
    Queued {
        message_id: MessageId,
        delivered: bool,
        parent: String,
    },
    NoParent,
    ParentEnded,
    SiblingsRunning,
    NothingToReport,
    ChildMissing,
    NotRequested,
}

pub(super) fn report_fleet(
    workspace: &ResolvedWorkspace,
    store: &Store,
    parent_id: &AgentSessionId,
) -> Result<ReportOutcome, ReportErr> {
    report_fleet_with_kind(workspace, store, parent_id, None)
}

pub(super) fn report_parent<'a>(
    agents: &'a [AgentState],
    parent_id: &AgentSessionId,
    parent_kind: Option<&AgentKind>,
) -> Option<&'a AgentState> {
    let kind = match parent_kind {
        Some(kind) => kind,
        None => {
            &agents
                .iter()
                .find(|agent| agent.answers_to(parent_id))?
                .kind
        }
    };
    rimz::address::launch_row(agents, kind, parent_id)
}

pub(super) fn report_fleet_with_kind(
    workspace: &ResolvedWorkspace,
    store: &Store,
    parent_id: &AgentSessionId,
    parent_kind: Option<&AgentKind>,
) -> Result<ReportOutcome, ReportErr> {
    let mut projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(parent) = report_parent(&projection.agents, parent_id, parent_kind) else {
        return Ok(ReportOutcome::NoParent);
    };
    if parent.ended_at.is_some() {
        return Ok(ReportOutcome::ParentEnded);
    }
    // A launcher with no members has nothing to report whatever the runs
    // directory holds, and a parked run asks this on every strand poll.
    if rimz::harness::fleet::reporting_fleet(&projection.agents, parent).is_empty() {
        return Ok(ReportOutcome::NothingToReport);
    }

    let runs = run::list(store.paths())?;
    // A held line names the provider's label, which only the stored context carries.
    let held_children = runs
        .iter()
        .filter(|run| FleetRuns::is_held(run))
        .filter_map(|run| Some((&run.kind, run.agent_id.as_ref()?)))
        .collect::<Vec<_>>();
    rimz::store::agent_context::attach_child_display_context(
        store.runtime_paths(),
        projection.agents.iter_mut().filter(|agent| {
            held_children
                .iter()
                .any(|(kind, id)| **kind == agent.kind && **id == agent.agent_id)
        }),
    );
    let Some(parent) = report_parent(&projection.agents, parent_id, parent_kind) else {
        return Ok(ReportOutcome::NoParent);
    };
    let fleet = FleetRuns::of(&projection.agents, &runs, parent);
    let published = fleet
        .settled()
        .into_iter()
        .map(|(_, record)| {
            run::publish_response(store.paths(), record).map(|path| (&record.run_id, path))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    if fleet.any_working() {
        return Ok(ReportOutcome::SiblingsRunning);
    }
    let ordinal = |record: &RunRecord, answer: Option<&rimz::store::run::EarlierAnswer>| {
        answer.map_or(record.follow_ups + 1, |answer| answer.ordinal)
    };
    let (rows, dropped): (Vec<_>, Vec<_>) = fleet
        .unreported()
        .into_iter()
        .flat_map(|(child, record)| {
            record
                .answer_claims()
                .filter(|claim| claim.owed)
                .map(move |claim| (child, record, claim.earlier))
        })
        .partition(|(child, record, answer)| {
            let status = answer.map_or(record.status, |answer| answer.status);
            rimz::harness::fleet::reports_outcome(child, status)
        });
    // An answer its booking's `on` drops is dismissed, so no later digest carries it.
    for (_, record, answer) in &dropped {
        run::report::join_and_settle_digest(
            store,
            &workspace.session_name,
            &record.run_id,
            Some(ordinal(record, *answer)),
            "outside its booking's `on`",
        )?;
    }
    if rows.is_empty() {
        return Ok(ReportOutcome::NothingToReport);
    }

    let responses = rows
        .iter()
        .map(|(_, record, answer)| {
            let path = match answer {
                Some(answer) => run::earlier_response_path(store.paths(), record, answer.ordinal),
                None => published
                    .get(&record.run_id)
                    .and_then(Option::as_ref)
                    .cloned(),
            };
            let Some(path) = path else {
                return Ok(None);
            };
            let summary = match FileSummary::measure(&path) {
                Ok(summary) => summary,
                Err(error) if answer.is_some() && error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(source) => return Err(ReportErr::Response { path, source }),
            };
            Ok(Some(ResponseFile { path, summary }))
        })
        .collect::<Result<Vec<_>, ReportErr>>()?;
    let clock = Clock::local();
    let accounts = ResumeAccounts::new(workspace, store, clock.now);
    let digest_rows = rows
        .iter()
        .zip(&responses)
        .map(|((child, run, answer), response)| {
            let failed_on_limit = answer
                .map_or(run.limit_failure.as_ref(), |answer| {
                    answer.limit_failure.as_ref()
                })
                .is_some();
            let account = if failed_on_limit {
                accounts.of(child)
            } else {
                ResumeAccount::Same
            };
            (*child, *run, *answer, response.as_ref(), account)
        })
        .collect::<Vec<_>>();
    let held = fleet.held();

    let subagents =
        rows.iter().all(|(_, run, _)| run.subagent) && held.iter().all(|(_, run)| run.subagent);
    let sender = MessageSender::Harness {
        notice: if subagents {
            HarnessNotice::SubagentReport
        } else {
            HarnessNotice::AgentReport
        },
    };
    let pane_id = parent.pane.as_ref().map(|pane| &pane.pane_id);
    let message = SyntheticMessage {
        agent: parent,
        text: compose_digest(&digest_rows, &held, subagents, &clock),
        sender,
        gate: DeliveryGate::Done,
        premises: rimz::store::message::Premise::NONE,
    }
    .record(workspace);
    let message_id = message.message_id.clone();
    let answers = rows
        .iter()
        .map(|(_, run, answer)| (run.run_id.clone(), ordinal(run, *answer)))
        .collect::<Vec<_>>();
    if !run::report::record_report_messages(store.paths(), &answers, Some(&message_id))? {
        return Ok(ReportOutcome::NothingToReport);
    }
    if !queue_unless_joined(workspace, store, &message, &answers)? {
        return Ok(ReportOutcome::Queued {
            message_id,
            delivered: false,
            parent: parent
                .name
                .clone()
                .unwrap_or_else(|| parent.agent_id.to_string()),
        });
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
    Ok(ReportOutcome::Queued {
        message_id,
        delivered,
        parent: parent
            .name
            .clone()
            .unwrap_or_else(|| parent.agent_id.to_string()),
    })
}

/// Queue `message`, the digest `answers` were just claimed for, and say whether it stays queued:
/// a join landing between the claim and the queue write found no record to withdraw, so the
/// digest is withdrawn here once every answer it reports is joined.
fn queue_unless_joined(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
    answers: &[(RunId, u32)],
) -> Result<bool, ReportErr> {
    if let Err(err) = store.queue_message(message, &workspace.session_name) {
        let _ = run::report::record_report_messages(store.paths(), answers, None);
        return Err(err.into());
    }
    if !run::report::digest_fully_joined(store.paths(), &message.message_id)? {
        return Ok(true);
    }
    store.withdraw_messages(&workspace.session_name, |record| {
        (record.message_id == message.message_id).then(|| "joined inline".to_owned())
    })?;
    Ok(false)
}

pub(super) fn report_settled_child(
    workspace: &ResolvedWorkspace,
    store: &Store,
    run: &RunRecord,
) -> Result<ReportOutcome, ReportErr> {
    if !run.status.is_terminal() {
        return Ok(ReportOutcome::NotRequested);
    }
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(child) = projection.agents.iter().find(|agent| {
        agent.kind == run.kind
            && run.agent_id.as_ref().map_or_else(
                || agent.name.as_deref() == run.agent_name.as_deref(),
                |agent_id| &agent.agent_id == agent_id,
            )
    }) else {
        return Ok(ReportOutcome::ChildMissing);
    };
    let Some((launcher_kind, launcher_id)) = rimz::harness::fleet::report_recipient(child) else {
        return Ok(ReportOutcome::NoParent);
    };
    report_fleet_with_kind(workspace, store, launcher_id, Some(launcher_kind))
}

pub(super) fn backstop_digest(request: super::SubagentDigestRequest) -> anyhow::Result<()> {
    let ctx = super::Ctx::for_workspace(request.workspace_id, None)
        .context("resolving subagent digest workspace")?;
    if let Err(error) =
        super::team_report::settle_ended_teams(&ctx.workspace, &ctx.store, &request.parent_agent_id)
    {
        tracing::debug!(%error, "could not settle ended team runs");
    }
    settle_peer_turns(&ctx.store, &request.parent_agent_id)
        .context("settling abandoned peer turns")?;
    let outcome = report_fleet(&ctx.workspace, &ctx.store, &request.parent_agent_id)
        .context("reconstructing subagent fleet digest")?;
    let ReportOutcome::Queued { message_id, .. } = outcome else {
        return Ok(());
    };
    rimz::diag::DiagSink::for_workspace(
        ctx.workspace.workspace_id.clone(),
        ctx.workspace.session_name.clone(),
        None,
    )
    .emit(rimz::diag::record::DiagEvent::SubagentDigestBackstopped {
        parent_agent_id: request.parent_agent_id,
        message_id,
    });
    Ok(())
}

fn settle_peer_turns(store: &Store, parent_id: &AgentSessionId) -> Result<(), ReportErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(parent) = report_parent(&projection.agents, parent_id, None) else {
        return Ok(());
    };
    let mut stranded = Vec::new();
    let agents = &projection.agents;
    for peer in rimz::harness::fleet::reporting_fleet(agents, parent)
        .into_iter()
        .filter(|peer| rimz::address::is_launch_row(agents, peer))
    {
        let Some(record) = run::open_peer_run(store.paths(), peer)? else {
            continue;
        };
        if peer.ended_at.is_some()
            || rimz::store::runtime::agent_liveness(peer)
                == rimz::store::runtime::AgentLiveness::Dead
        {
            run::fail_peer_run(store, peer, "peer session ended")?;
            continue;
        }
        if let run::ParkCheck::Stranded(at) = run::settle_stranded_park(store, &record, None)? {
            stranded.push((record.run_id, at));
        }
    }
    if stranded.is_empty() {
        return Ok(());
    }
    // Match the wrapper's strand cadence, allowing stamp-before-queue producers to finish.
    std::thread::sleep(std::time::Duration::from_secs(5));
    for (id, at) in stranded {
        let record = run::load(store.paths(), &id)?;
        run::settle_stranded_park(store, &record, Some(at))?;
    }
    Ok(())
}

/// Reads the room's accounts once, and only for a row that failed on a limit.
struct ResumeAccounts<'a> {
    workspace: &'a ResolvedWorkspace,
    store: &'a Store,
    now: Timestamp,
    logins: std::cell::OnceCell<(
        Option<rimz::agents::RoomAccounts>,
        rimz::agents::LoginCatalog,
    )>,
}

impl<'a> ResumeAccounts<'a> {
    fn new(workspace: &'a ResolvedWorkspace, store: &'a Store, now: Timestamp) -> Self {
        Self {
            workspace,
            store,
            now,
            logins: std::cell::OnceCell::new(),
        }
    }

    fn of(&self, child: &AgentState) -> ResumeAccount {
        let (logins, catalog) = self.logins.get_or_init(|| {
            let logins = rimz::agents::room_accounts(
                &self.store.paths().workspace_record,
                Some(&self.workspace.project_root),
                &rimz::config::MachineConfig::load_lenient(),
            );
            (logins.ok(), rimz::agents::machine_login_catalog())
        });
        match rimz::harness::resume::resume_login(child, logins.as_ref(), catalog) {
            rimz::harness::resume::ResumeLogin::Same => ResumeAccount::Same,
            rimz::harness::resume::ResumeLogin::Unreadable => ResumeAccount::Unreadable,
            rimz::harness::resume::ResumeLogin::Changed(key) => {
                ResumeAccount::Changed(rimz::harness::park_notice::refill_ahead(
                    self.store.runtime_paths(),
                    &key,
                    child.model.as_deref(),
                    self.now,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests;
