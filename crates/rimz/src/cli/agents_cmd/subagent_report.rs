//! Durable parent-facing completion digests for launched subagent fleets.
//!
//! Response files land per child; run records are stamped per fleet before
//! the digest enters the message queue. This makes the complete row set
//! visible to inline join cancellation and lets the wrapper fast path race
//! safely with the producer backstop.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use rimz::agents::AgentState;
use rimz::disk::summary::FileSummary;
use rimz::harness::fleet::FleetRuns;
use rimz::harness::run;
use rimz::ids::{AgentKind, AgentSessionId, MessageId};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};
use rimz::store::run::{EarlierAnswer, RunRecord, RunStatus, RunStoreErr};
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

#[derive(Debug, thiserror::Error)]
pub(super) enum ReportErr {
    #[error(transparent)]
    Store(#[from] rimz::store::StoreErr),
    #[error(transparent)]
    Run(#[from] RunStoreErr),
    #[error(transparent)]
    Deliver(#[from] rimz::message::deliver::DeliverErr),
    #[error(transparent)]
    Publish(#[from] run::ResponsePublishErr),
    #[error("measuring subagent response file {path}: {source}")]
    Response {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(super) struct ResponseFile {
    pub(super) path: PathBuf,
    pub(super) summary: FileSummary,
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
                .find(|agent| {
                    &agent.agent_id == parent_id || agent.launch_id.as_ref() == Some(parent_id)
                })?
                .kind
        }
    };
    rimz::address::launch_row(agents, kind, parent_id)
}

fn report_fleet_with_kind(
    workspace: &ResolvedWorkspace,
    store: &Store,
    parent_id: &AgentSessionId,
    parent_kind: Option<&AgentKind>,
) -> Result<ReportOutcome, ReportErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(parent) = report_parent(&projection.agents, parent_id, parent_kind) else {
        return Ok(ReportOutcome::NoParent);
    };
    if parent.ended_at.is_some() {
        return Ok(ReportOutcome::ParentEnded);
    }
    // A launcher with no members has nothing to report whatever the runs
    // directory holds, and a parked run asks this on every strand poll.
    if rimz::address::launched_fleet(&projection.agents, parent).is_empty() {
        return Ok(ReportOutcome::NothingToReport);
    }

    let runs = run::list(store.paths())?;
    let fleet = FleetRuns::of(&projection.agents, &runs, parent);
    let published = fleet
        .settled()
        .into_iter()
        .map(|(_, record)| {
            run::publish_response(store.paths(), record).map(|path| (&record.run_id, path))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    if fleet.any_running() {
        return Ok(ReportOutcome::SiblingsRunning);
    }
    let rows = fleet
        .unreported()
        .into_iter()
        .flat_map(|(child, record)| {
            record
                .answer_claims()
                .filter(|claim| claim.owed)
                .map(move |claim| (child, record, claim.earlier))
        })
        .collect::<Vec<_>>();
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
    let digest_rows = rows
        .iter()
        .zip(&responses)
        .map(|((child, run, answer), response)| (*child, *run, *answer, response.as_ref()))
        .collect::<Vec<_>>();

    let subagents = rows.iter().all(|(_, run, _)| run.subagent);
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
        text: compose_digest(&digest_rows, subagents),
        sender,
        gate: DeliveryGate::Done,
        pane_id: pane_id.cloned(),
    }
    .record(workspace);
    let message_id = message.message_id.clone();
    let answers = rows
        .iter()
        .map(|(_, run, answer)| {
            (
                run.run_id.clone(),
                answer.map_or(run.follow_ups + 1, |answer| answer.ordinal),
            )
        })
        .collect::<Vec<_>>();
    if !run::report::record_report_messages(store.paths(), &answers, Some(&message_id))? {
        return Ok(ReportOutcome::NothingToReport);
    }
    if let Err(err) = store.queue_message(&message, &workspace.session_name) {
        let _ = run::report::record_report_messages(store.paths(), &answers, None);
        return Err(err.into());
    }
    if run::report::digest_fully_joined(store.paths(), &message_id)? {
        store.cancel_message(&message_id, &workspace.session_name, "joined inline")?;
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
    let Some((launcher_kind, launcher_id)) = child.launcher() else {
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
    for peer in rimz::address::launched_fleet(agents, parent)
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

type DigestRow<'a> = (
    &'a AgentState,
    &'a RunRecord,
    Option<&'a EarlierAnswer>,
    Option<&'a ResponseFile>,
);

fn compose_digest(rows: &[DigestRow<'_>], subagents: bool) -> String {
    let noun = if subagents {
        "subagent"
    } else {
        "background agent"
    };
    let responses = rows
        .iter()
        .filter_map(|(_, _, _, response)| response.map(|response| response.summary))
        .collect::<Vec<_>>();
    let heading = if rows.len() == 1 {
        format!("Your {noun} settled:")
    } else if responses.len() >= 2 {
        let total = responses.into_iter().sum::<FileSummary>();
        format!(
            "All {} {noun}s settled, responses total {}:",
            rows.len(),
            total.label()
        )
    } else {
        format!("All {} {noun}s settled:", rows.len())
    };
    let rows = rows
        .iter()
        .map(|(child, run, answer, response)| compose_answer_row(child, run, *answer, *response))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{heading}\n{rows}")
}

pub(super) fn compose_digest_row(
    child: &AgentState,
    run: &RunRecord,
    response: Option<&ResponseFile>,
) -> String {
    compose_answer_row(child, run, None, response)
}

fn compose_answer_row(
    child: &AgentState,
    run: &RunRecord,
    answer: Option<&EarlierAnswer>,
    response: Option<&ResponseFile>,
) -> String {
    let status = answer.map_or(run.status, |answer| answer.status);
    let started_at = answer.map_or_else(
        || {
            run.follow_up
                .as_ref()
                .map_or(run.started_at, |turn| turn.started_at)
        },
        |answer| answer.started_at,
    );
    let finished_at = answer
        .map_or(run.completed_at, |answer| answer.completed_at)
        .unwrap_or(run.updated_at);
    let follow_up = answer.map_or(run.follow_ups > 0, |answer| answer.ordinal > 1);
    let prompt = if follow_up {
        match answer {
            Some(answer) => answer.prompt.as_deref(),
            None => run
                .follow_up
                .as_ref()
                .and_then(|turn| turn.prompt.as_deref()),
        }
    } else {
        Some(run.prompt.as_str())
    };
    let elapsed =
        format_compact_duration(finished_at.duration_since(started_at).as_secs().max(0) as u64);
    let preposition = if status == RunStatus::TimedOut {
        "after"
    } else {
        "in"
    };
    let mut row = format!(
        "- @{}: {} {preposition} {elapsed}",
        child_name(child, run),
        status.label(),
    );
    if status != RunStatus::Completed
        && let Some(reason) = failure_reason(answer.map_or(run.failure_tail.as_deref(), |answer| {
            answer.failure_tail.as_deref()
        }))
    {
        row.push_str("; ");
        row.push_str(reason);
    }
    if let Some(task) = child
        .description
        .as_deref()
        .filter(|value| !follow_up && run.peer.is_none() && !value.is_empty())
        .map(std::borrow::Cow::Borrowed)
        .or_else(|| {
            prompt?
                .lines()
                .next()
                .filter(|line| !line.is_empty())
                .map(rimz::theme::fmt::command_preview)
        })
    {
        row.push_str(&format!(", task: \"{task}\""));
    }
    match response {
        Some(response) => row.push_str(&format!(
            ", {}response: {} ({})",
            if status == RunStatus::TimedOut {
                "partial "
            } else {
                ""
            },
            response.path.display(),
            response.summary.label(),
        )),
        None => row.push_str(", no response"),
    }
    row
}

fn format_compact_duration(mut seconds: u64) -> String {
    let mut rendered = String::new();
    for (unit_seconds, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        let amount = seconds / unit_seconds;
        if amount > 0 {
            rendered.push_str(&format!("{amount}{suffix}"));
            seconds %= unit_seconds;
        }
    }
    if seconds > 0 || rendered.is_empty() {
        rendered.push_str(&format!("{seconds}s"));
    }
    rendered
}

fn child_name<'a>(child: &'a AgentState, run: &'a RunRecord) -> &'a str {
    child
        .name
        .as_deref()
        .or(run.agent_name.as_deref())
        .unwrap_or_else(|| child.agent_id.as_str())
}

fn failure_reason(tail: Option<&str>) -> Option<&str> {
    tail?
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
}

#[cfg(test)]
mod tests;
