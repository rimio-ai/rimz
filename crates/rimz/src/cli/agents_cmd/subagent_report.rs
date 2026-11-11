//! Durable parent-facing completion digests for launched subagent fleets.
//!
//! Response files land per child; run records are stamped per fleet before
//! the digest enters the message queue. This makes the complete row set
//! visible to inline join cancellation and lets the wrapper fast path race
//! safely with the producer backstop.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use rimz::agents::AgentState;
use rimz::disk::summary::FileSummary;
use rimz::harness::fleet::FleetRuns;
use rimz::harness::run;
use rimz::ids::{AgentKind, AgentSessionId, MessageId};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};
use rimz::store::run::{
    EarlierAnswer, LimitFailure, LimitFailureCause, RunRecord, RunStatus, RunStoreErr,
};
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimeScope, Store};

use crate::cli::render;

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
    if rimz::address::launched_fleet(&projection.agents, parent).is_empty() {
        return Ok(ReportOutcome::NothingToReport);
    }

    let runs = run::list(store.paths())?;
    // A held line names the provider's label, which only the stored context carries.
    let held_children = runs
        .iter()
        .filter(|run| FleetRuns::is_held(run))
        .filter_map(|run| Some((&run.kind, run.agent_id.as_ref()?)))
        .collect::<Vec<_>>();
    rimz::store::agent_context::attach_rest_certificates(
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

/// The account a resume of a limit-failed child would use, against the one it stopped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeAccount {
    /// The same account, or one whose lookup failed for another reason.
    Same,
    /// Another account that can read the session, with its refill if one is ahead.
    Changed(Option<Timestamp>),
    /// An account that cannot read the session.
    Unreadable,
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

/// The instant and zone a report is composed in.
struct Clock {
    now: Timestamp,
    tz: TimeZone,
}

impl Clock {
    fn local() -> Self {
        Self {
            now: Timestamp::now(),
            tz: TimeZone::system(),
        }
    }
}

type DigestRow<'a> = (
    &'a AgentState,
    &'a RunRecord,
    Option<&'a EarlierAnswer>,
    Option<&'a ResponseFile>,
    ResumeAccount,
);

fn compose_digest(
    rows: &[DigestRow<'_>],
    held: &[(&AgentState, &RunRecord)],
    subagents: bool,
    clock: &Clock,
) -> String {
    let noun = if subagents {
        "subagent"
    } else {
        "background agent"
    };
    let responses = rows
        .iter()
        .filter_map(|(_, _, _, response, _)| response.map(|response| response.summary))
        .collect::<Vec<_>>();
    let heading = if !held.is_empty() {
        let settled = rows.len();
        let total = settled + held.len();
        match responses.len() {
            2.. => format!(
                "{settled} of {total} {noun}s settled, responses total {}:",
                responses.into_iter().sum::<FileSummary>().label()
            ),
            _ => format!("{settled} of {total} {noun}s settled:"),
        }
    } else if rows.len() == 1 {
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
        .map(|(child, run, answer, response, account)| {
            compose_answer_row(child, run, *answer, *response, *account, clock)
        })
        .chain(
            held.iter()
                .map(|(child, run)| compose_held_line(child, run, clock)),
        )
        .collect::<Vec<_>>()
        .join("\n");
    format!("{heading}\n{rows}")
}

pub(super) fn compose_digest_row(
    child: &AgentState,
    run: &RunRecord,
    response: Option<&ResponseFile>,
) -> String {
    compose_answer_row(
        child,
        run,
        None,
        response,
        ResumeAccount::Same,
        &Clock::local(),
    )
}

fn compose_answer_row(
    child: &AgentState,
    run: &RunRecord,
    answer: Option<&EarlierAnswer>,
    response: Option<&ResponseFile>,
    account: ResumeAccount,
    clock: &Clock,
) -> String {
    let status = answer.map_or(run.status, |answer| answer.status);
    let started_at = answer.map_or_else(|| run.answer_started_at(), |answer| answer.started_at);
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
    let elapsed = render::format_compact_duration(
        finished_at.duration_since(started_at).as_secs().max(0) as u64,
    );
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
    let limit_failure = answer.map_or(run.limit_failure.as_ref(), |answer| {
        answer.limit_failure.as_ref()
    });
    if status != RunStatus::Completed
        && let Some(failure) = limit_failure
    {
        row.push_str("; ");
        row.push_str(&limit_reason(failure, &child.kind, account, clock));
    } else if status != RunStatus::Completed
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

/// A limit failure's reason: the provider's label, then what the parent can do now.
fn limit_reason(
    failure: &LimitFailure,
    kind: &AgentKind,
    account: ResumeAccount,
    clock: &Clock,
) -> String {
    let mut reason = "stopped on a provider limit".to_owned();
    if let Some(label) = &failure.label {
        reason.push_str(&format!(": \"{label}\""));
    }
    let refills_in = |at| render::refill_in(at, clock.now, &clock.tz);
    let ahead = failure.refill_at.filter(|at| *at > clock.now);
    reason.push_str(&match (account, failure.cause) {
        (ResumeAccount::Unreadable, _) => format!(
            ". The room's {kind} account has changed and cannot read its session: do the task yourself"
        ),
        (ResumeAccount::Changed(None), _) => format!(
            ". The room's {kind} account has changed since it stopped: message it to retry now; if that fails too, do the task yourself"
        ),
        (ResumeAccount::Changed(Some(at)), _) => format!(
            "; it refills in {}. Message it after the refill to continue it",
            refills_in(at)
        ),
        (ResumeAccount::Same, LimitFailureCause::ContinueFailed) => {
            ", and continuing it after the refill failed. Message it to retry; if that fails too, do the task yourself".to_owned()
        }
        (ResumeAccount::Same, LimitFailureCause::RefillUnknown) => {
            ". Message it to retry; if that fails too, do the task yourself".to_owned()
        }
        (ResumeAccount::Same, LimitFailureCause::BeyondHoldLimit) if let Some(at) = ahead => {
            format!(
                "; it refills in {}. Do the task yourself; a message after the refill continues it",
                refills_in(at)
            )
        }
        (ResumeAccount::Same, LimitFailureCause::RefillKnown) if let Some(at) = ahead => format!(
            "; it refills in {}. Message it after the refill to continue it",
            refills_in(at)
        ),
        (ResumeAccount::Same, _) => "; the limit has refilled. Message it to continue it".to_owned(),
    });
    reason
}

/// A child held for its provider's refill, which a later report answers for.
fn compose_held_line(child: &AgentState, run: &RunRecord, clock: &Clock) -> String {
    let mut line = format!("- @{}: paused on a provider limit", child_name(child, run));
    if let Some(label) = child
        .displayed_turn_error(clock.now)
        .and_then(|(_, error)| error.label.as_deref())
    {
        line.push_str(&format!(": \"{label}\""));
    }
    match run
        .limit_hold
        .map(|hold| hold.until)
        .filter(|at| *at > clock.now)
    {
        Some(at) => line.push_str(&format!(
            "; it refills in {} and RimZ continues it then",
            render::refill_in(at, clock.now, &clock.tz)
        )),
        None => line.push_str("; the limit has refilled and RimZ is continuing it"),
    }
    line.push_str(". Its report follows");
    line
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
