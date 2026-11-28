//! Durable membership markers for parent-owned results and subagent report digests.
//!
//! An answer printed to a human shell, or to its launcher during the
//! launcher's open turn, or dismissed by the parent's `rimz subagents stop`,
//! is excluded from a digest that has not been composed. Any other agent that
//! prints an answer leaves it to the digest. Once a digest exists, it
//! may be canceled only after every listed answer has been claimed this way,
//! preserving the notice while any row remains unread. A launcher's join on
//! runs that report to it is not held: it prints report rows and leaves an
//! answer still running to the digest. The digest's text, one row per answer, is composed
//! here too, for the fleet report and the team report alike.

use std::path::PathBuf;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::agents::AgentState;
use crate::disk::paths::StatePaths;
use crate::disk::summary::FileSummary;
use crate::harness::ancestry::{CallerIdentity, resolve_launch_caller};
use crate::ids::{AgentKind, MessageId, RunId};
use crate::store::Store;
use crate::store::run::{EarlierAnswer, LimitFailure, LimitFailureCause, RunStatus, RunStoreErr};
use crate::store::snapshot::SidebarSnapshot;

use super::{RecordMutation, Result, RunRecord, update_record};

/// Whether a join that printed `record` to `caller` claims it: a person at a
/// shell (`caller` None) does, and an agent does only once it resolves to a row
/// that holds an open turn and launched an agent row the record matches. `audit`
/// holds every durable row, as the fleet digest reads them (`RuntimeScope::Audit`).
pub fn may_claim(
    snapshot: &SidebarSnapshot,
    audit: &[AgentState],
    caller: Option<&CallerIdentity>,
    record: &RunRecord,
) -> bool {
    Claimant::resolve(&snapshot.agents, caller).is_some_and(|claimant| claimant.owns(audit, record))
}

/// Whether a join by `caller` on `record` leaves the answer to the fleet report that will
/// carry it: the run reports to its launcher, and `caller` resolves to a live row that
/// launched an agent row the record matches. Unlike a claim, it needs no open turn. `audit`
/// holds every durable row, as for [`may_claim`].
pub fn defers_to_report(
    agents: &[AgentState],
    audit: &[AgentState],
    caller: Option<&CallerIdentity>,
    record: &RunRecord,
) -> bool {
    crate::harness::fleet::FleetRuns::reports_to_launcher(record)
        && caller
            .and_then(|caller| resolve_launch_caller(agents, caller).ok())
            .filter(|launcher| launcher.ended_at.is_none())
            .is_some_and(|launcher| launched(audit, launcher, record))
}

/// Whether `launcher` launched an agent row `record` matches, searched among every durable
/// row: a `-p --bg` run's row leaves the live snapshot once its session ends, while the fleet
/// digest still reports the run from it.
fn launched(audit: &[AgentState], launcher: &AgentState, record: &RunRecord) -> bool {
    audit
        .iter()
        .any(|child| record.matches_agent(child) && child.launcher_is(launcher))
}

/// Who a printed answer is claimed for; an unresolved or unattended agent
/// caller claims nothing and leaves its answers to the digest.
enum Claimant<'a> {
    Person,
    Launcher(&'a AgentState),
}

impl<'a> Claimant<'a> {
    fn resolve(agents: &'a [AgentState], caller: Option<&CallerIdentity>) -> Option<Self> {
        let Some(caller) = caller else {
            return Some(Self::Person);
        };
        resolve_launch_caller(agents, caller)
            .ok()
            .filter(|parent| parent.holds_open_turn())
            .map(Self::Launcher)
    }

    fn owns(&self, audit: &[AgentState], record: &RunRecord) -> bool {
        match self {
            Self::Person => true,
            Self::Launcher(parent) => launched(audit, parent, record),
        }
    }
}

/// Join one settled ordinal; `None` dismisses every answer, including a live one.
pub fn join_and_settle_digest(
    store: &Store,
    session_name: &str,
    run_id: &RunId,
    ordinal: Option<u32>,
    reason: &str,
) -> crate::store::Result<()> {
    for message_id in mark_joined_record(store.paths(), run_id, ordinal)? {
        if digest_fully_joined(store.paths(), &message_id)? {
            store.withdraw_messages(session_name, |message| {
                (message.message_id == message_id).then(|| reason.to_owned())
            })?;
        }
    }
    Ok(())
}

fn mark_joined_record(
    paths: &StatePaths,
    run_id: &RunId,
    ordinal: Option<u32>,
) -> Result<Vec<MessageId>> {
    update_record(paths, run_id, |record, now| {
        let mut messages = Vec::new();
        let mut changed = false;
        let mut join = |report: &Option<MessageId>, joined: &mut Option<jiff::Timestamp>| {
            if let Some(message_id) = report
                && !messages.contains(message_id)
            {
                messages.push(message_id.clone());
            }
            if joined.is_none() {
                *joined = Some(now);
                changed = true;
            }
        };
        match ordinal {
            Some(ordinal) => {
                if let Some((report, joined)) = answer_claims_mut(record, ordinal) {
                    join(report, joined);
                }
            }
            None => {
                let ordinals = record
                    .answer_claims()
                    .map(|claim| claim.ordinal)
                    .collect::<Vec<_>>();
                for ordinal in ordinals {
                    if let Some((report, joined)) = answer_claims_mut(record, ordinal) {
                        join(report, joined);
                    }
                }
                // Dismissal also covers the current answer while it is still live.
                join(&record.report_message_id, &mut record.joined_at);
            }
        }
        Ok(if changed {
            RecordMutation::Write(messages)
        } else {
            RecordMutation::Keep(messages)
        })
    })
    .map(|(_, messages)| messages)
}

fn answer_claims_mut(
    record: &mut RunRecord,
    ordinal: u32,
) -> Option<(&mut Option<MessageId>, &mut Option<jiff::Timestamp>)> {
    if ordinal == record.follow_ups + 1 {
        return record
            .status
            .is_terminal()
            .then_some((&mut record.report_message_id, &mut record.joined_at));
    }
    record
        .earlier_answers
        .iter_mut()
        .find(|answer| answer.ordinal == ordinal && answer.status.is_terminal())
        .map(|answer| (&mut answer.report_message_id, &mut answer.joined_at))
}

pub fn record_report_messages(
    paths: &StatePaths,
    answers: &[(RunId, u32)],
    message_id: Option<&MessageId>,
) -> Result<bool> {
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut seen = std::collections::HashSet::new();
    let originals = answers
        .iter()
        .filter(|(run_id, _)| seen.insert(run_id))
        .map(|(run_id, _)| super::load(paths, run_id))
        .collect::<Result<Vec<_>>>()?;
    if message_id.is_some() && originals.iter().any(|record| !record.status.is_terminal()) {
        return Ok(false);
    }
    let mut records = originals.clone();
    for record in &mut records {
        for (run_id, ordinal) in answers {
            if *run_id != record.run_id {
                continue;
            }
            let Some((report, joined)) = answer_claims_mut(record, *ordinal) else {
                if message_id.is_some() {
                    return Ok(false);
                }
                continue;
            };
            if message_id.is_some() && (report.is_some() || joined.is_some()) {
                return Ok(false);
            }
            *report = message_id.cloned();
        }
    }
    let mut written: Vec<RunRecord> = Vec::new();
    let mut write_error = None;
    let now = jiff::Timestamp::now();
    for (original, record) in originals.iter().zip(&mut records) {
        if original == record {
            continue;
        }
        record.updated_at = now;
        match crate::store::run::write(&paths.runs_dir, record) {
            Ok(()) => written.push(original.clone()),
            Err(err) => {
                write_error.get_or_insert(err);
            }
        }
    }
    if let Some(write_error) = write_error {
        let mut rollback_error = None;
        for original in written.iter().rev() {
            if let Err(err) = crate::store::run::write(&paths.runs_dir, original) {
                rollback_error.get_or_insert(err);
            }
        }
        return Err(rollback_error.unwrap_or(write_error));
    }
    Ok(true)
}

pub fn digest_fully_joined(paths: &StatePaths, message_id: &MessageId) -> Result<bool> {
    let mut found = false;
    for record in super::list(paths)? {
        for claim in record.answer_claims() {
            if claim.report == Some(message_id) {
                found = true;
                if claim.joined.is_none() {
                    return Ok(false);
                }
            }
        }
    }
    Ok(found)
}

/// Why a report could not be published, composed, or queued.
#[derive(Debug, thiserror::Error)]
pub enum ReportErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Run(#[from] RunStoreErr),
    #[error(transparent)]
    Deliver(#[from] crate::message::deliver::DeliverErr),
    #[error(transparent)]
    Publish(#[from] super::ResponsePublishErr),
    #[error("measuring subagent response file {path}: {source}")]
    Response {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A published response file and its measured size, as a digest row names it.
pub struct ResponseFile {
    pub path: PathBuf,
    pub summary: FileSummary,
}

/// The account a resume of a limit-failed child would use, against the one it stopped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeAccount {
    /// The same account, or one whose lookup failed for another reason.
    Same,
    /// Another account that can read the session, with its refill if one is ahead.
    Changed(Option<Timestamp>),
    /// An account that cannot read the session.
    Unreadable,
}

/// The instant and zone a report is composed in.
pub struct Clock {
    pub now: Timestamp,
    pub tz: TimeZone,
}

impl Clock {
    pub fn local() -> Self {
        Self {
            now: Timestamp::now(),
            tz: TimeZone::system(),
        }
    }
}

/// One answer a digest reports: the child, its run, the earlier answer it reports in place of
/// the run's current one, the response file, and the account a resume would use.
pub type DigestRow<'a> = (
    &'a AgentState,
    &'a RunRecord,
    Option<&'a EarlierAnswer>,
    Option<&'a ResponseFile>,
    ResumeAccount,
);

/// The fleet digest: a heading over one row per answer and one line per child `held` for its
/// provider's refill.
pub fn compose_digest(
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

/// One settled answer's digest row, composed now in the local zone.
pub fn compose_digest_row(
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

pub fn compose_answer_row(
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
    let elapsed = crate::theme::fmt::format_compact_duration(
        finished_at.duration_since(started_at).as_secs().max(0) as u64,
    );
    let preposition = if status == RunStatus::TimedOut {
        "after"
    } else {
        "in"
    };
    // A fire's work reports to its booker, which tells two fires of one booking target apart
    // by the booking's id.
    let booking = child
        .loop_task
        .as_deref()
        .filter(|_| child.launcher().is_none() && child.loop_fire.is_some())
        .map(|task| format!(" (schedule:{task})"))
        .unwrap_or_default();
    let mut row = format!(
        "- @{}{booking}: {} {preposition} {elapsed}",
        child_name(child, run),
        status.label(),
    );
    let limit_failure = answer.map_or(run.limit_failure.as_ref(), |answer| {
        answer.limit_failure.as_ref()
    });
    let cancel_reason = answer.map_or(run.cancel_reason, |answer| answer.cancel_reason);
    if status == RunStatus::Canceled
        && let Some(reason) = cancel_reason
    {
        row.push_str("; ");
        row.push_str(reason.label());
    } else if status != RunStatus::Completed
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
                .map(crate::theme::fmt::command_preview)
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
    let refills_in = |at| crate::theme::fmt::refill_in(at, clock.now, &clock.tz);
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
            crate::theme::fmt::refill_in(at, clock.now, &clock.tz)
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
mod tests {
    use std::path::Path;

    use crate::agents::{LifecycleSignal, PermissionMode};
    use crate::disk::paths::RuntimePaths;
    use crate::ids::{AgentKind, WorkspaceId};
    use crate::store::run::RunStatus;

    use super::*;

    #[test]
    fn joined_and_report_fields_are_first_writer_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/run-report"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let mut record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "report".to_owned(),
            Path::new("/tmp/run-report").to_path_buf(),
        );
        record.status = crate::store::run::RunStatus::Completed;
        super::super::create(&paths, &record).expect("create run");

        let first = MessageId::new();
        let second = MessageId::new();
        let answers = [(record.run_id.clone(), 1)];
        assert!(record_report_messages(&paths, &answers, Some(&first)).unwrap());
        assert!(!record_report_messages(&paths, &answers, Some(&second)).unwrap());
        assert_eq!(
            super::super::load(&paths, &record.run_id)
                .unwrap()
                .report_message_id
                .as_ref(),
            Some(&first)
        );
        mark_joined_record(&paths, &record.run_id, Some(1)).expect("mark joined");
        let joined = super::super::load(&paths, &record.run_id).unwrap();
        mark_joined_record(&paths, &record.run_id, Some(1)).expect("repeat joined");
        assert_eq!(
            super::super::load(&paths, &record.run_id)
                .unwrap()
                .joined_at,
            joined.joined_at
        );
        let mut detached = record.clone();
        detached.run_id = RunId::new();
        detached.report_to = crate::store::run::ReportTo::Nobody;
        super::super::create(&paths, &detached).unwrap();
        assert!(
            mark_joined_record(&paths, &detached.run_id, Some(1))
                .unwrap()
                .is_empty()
        );
        assert!(
            super::super::load(&paths, &detached.run_id)
                .unwrap()
                .joined_at
                .is_some(),
            "an explicit join still claims a detached answer"
        );
        let mut sibling = record.clone();
        sibling.run_id = RunId::new();
        super::super::create(&paths, &sibling).unwrap();
        assert!(
            !record_report_messages(
                &paths,
                &[(sibling.run_id.clone(), 1), (record.run_id.clone(), 1)],
                Some(&second),
            )
            .unwrap()
        );
        assert!(
            super::super::load(&paths, &sibling.run_id)
                .unwrap()
                .report_message_id
                .is_none()
        );
        assert!(
            !record_report_messages(&paths, &[(sibling.run_id.clone(), 2)], Some(&second)).unwrap()
        );
        assert!(
            record_report_messages(&paths, &[(sibling.run_id.clone(), 1)], Some(&second)).unwrap()
        );
        record_report_messages(&paths, &[(sibling.run_id.clone(), 1)], None).unwrap();
        assert!(
            super::super::load(&paths, &sibling.run_id)
                .unwrap()
                .report_message_id
                .is_none()
        );
        assert_eq!(
            super::super::load(&paths, &record.run_id)
                .unwrap()
                .report_message_id,
            Some(first)
        );
    }

    #[test]
    fn digest_is_fully_joined_only_after_every_listed_run_is_joined() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/run-report-digest"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let message_id = MessageId::new();
        let records = ["first", "second"].map(|prompt| {
            let mut record = RunRecord::new(
                workspace_id.clone(),
                AgentKind::new_unchecked("codex"),
                PermissionMode::Auto,
                prompt.to_owned(),
                Path::new("/tmp/run-report-digest").to_path_buf(),
            );
            record.status = crate::store::run::RunStatus::Completed;
            super::super::create(&paths, &record).expect("create run");
            record_report_messages(&paths, &[(record.run_id.clone(), 1)], Some(&message_id))
                .expect("record digest");
            record
        });

        assert!(!digest_fully_joined(&paths, &message_id).expect("unjoined digest"));
        let mut first = records[0].clone();
        first.subagent = true;
        first.report_message_id = Some(message_id.clone());
        super::super::create(&paths, &first).unwrap();
        let observation = crate::agents::AgentLifecycleObservation::new(
            None,
            crate::agents::LifecycleSignal::TurnStarted { turn_id: None },
        );
        super::super::record_lifecycle(&paths, &first.run_id, "codex", &observation, None, || None)
            .unwrap();
        mark_joined_record(&paths, &first.run_id, Some(2)).expect("unsettled answer is not joined");
        assert!(
            super::super::load(&paths, &first.run_id)
                .unwrap()
                .joined_at
                .is_none()
        );
        mark_joined_record(&paths, &records[1].run_id, Some(1)).expect("join second");
        assert!(!digest_fully_joined(&paths, &message_id).expect("partially joined digest"));
        mark_joined_record(&paths, &first.run_id, Some(1)).expect("join the earlier answer");
        assert!(digest_fully_joined(&paths, &message_id).expect("fully joined digest"));
        assert!(!digest_fully_joined(&paths, &MessageId::new()).expect("unknown digest"));
    }

    #[test]
    fn may_claim_only_for_a_person_or_the_attended_launcher_and_defer_only_for_the_launcher() {
        use crate::store::run::{PeerRun, ReportTo};

        for answer in ["subagent", "peer"] {
            for caller_kind in [
                "parent",
                "human",
                "stranger",
                "unattended",
                "ended",
                "missing",
            ] {
                let (_dir, store, mut record, _) = claim_fixture();
                if answer == "peer" {
                    record.subagent = false;
                    record.peer = Some(PeerRun {
                        launch_id: "child".into(),
                        opened_by: std::mem::take(&mut record.opened_by),
                    });
                }
                let caller_name = match caller_kind {
                    "stranger" => "stranger",
                    "missing" => "missing",
                    _ => "parent",
                };
                if caller_kind == "unattended" {
                    append_claim_agent(
                        &store,
                        "parent",
                        LifecycleSignal::TurnEnded {
                            errored: false,
                            parked_on_background: false,
                            turn_id: None,
                        },
                        None,
                    );
                }
                if caller_kind == "ended" {
                    append_claim_agent(&store, "parent", LifecycleSignal::Ended, None);
                }
                let caller = (caller_kind != "human").then(|| CallerIdentity {
                    kind: AgentKind::new_unchecked("claude"),
                    launch_id: Some(caller_name.into()),
                    pane_id: None,
                    name: Some(caller_name.into()),
                    profile: None,
                    role: None,
                });
                let snapshot = store.snapshot_cached().unwrap();
                let audit = store
                    .runtime_projection(crate::store::runtime::RuntimeScope::Audit)
                    .unwrap()
                    .agents;
                assert_eq!(
                    may_claim(&snapshot, &audit, caller.as_ref(), &record),
                    matches!(caller_kind, "parent" | "human"),
                    "{answer}/{caller_kind}"
                );
                assert_eq!(
                    defers_to_report(&snapshot.agents, &audit, caller.as_ref(), &record),
                    matches!(caller_kind, "parent" | "unattended"),
                    "{answer}/{caller_kind} defers"
                );
                record.report_to = ReportTo::Nobody;
                assert!(
                    !defers_to_report(&snapshot.agents, &audit, caller.as_ref(), &record),
                    "{answer}/{caller_kind}: a detached run reports to nobody"
                );
            }
        }
    }

    fn append_claim_agent(
        store: &Store,
        name: &str,
        signal: LifecycleSignal,
        parent: Option<&str>,
    ) {
        let mut observation =
            crate::agents::AgentLifecycleObservation::new(Some(name.into()), signal);
        observation.agent_name = Some(name.into());
        observation.pane_id = Some(crate::ids::PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            match name {
                "parent" => "1",
                "stranger" => "2",
                _ => "3",
            },
        ));
        observation.launch.parent_agent_id = parent.map(Into::into);
        observation.launch.parent_agent_kind = parent.map(|_| AgentKind::new_unchecked("claude"));
        observation.launch.launch_depth = parent.map(|_| 1);
        store
            .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
                session_name: "test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }

    fn claim_fixture() -> (
        tempfile::TempDir,
        Store,
        crate::store::run::RunRecord,
        MessageId,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let store = Store::open(paths, runtime).unwrap();
        for name in ["parent", "stranger", "child"] {
            append_claim_agent(
                &store,
                name,
                LifecycleSignal::Registered,
                (name == "child").then_some("parent"),
            );
            append_claim_agent(
                &store,
                name,
                LifecycleSignal::TurnStarted { turn_id: None },
                None,
            );
        }
        let message_id = MessageId::new();
        let mut record = crate::store::run::RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            crate::agents::PermissionMode::Auto,
            "task".into(),
            dir.path().into(),
        );
        record.subagent = true;
        record.agent_id = Some("child".into());
        record.status = RunStatus::Completed;
        record.opened_by.push(message_id.clone());
        crate::harness::run::create(store.paths(), &record).unwrap();
        (dir, store, record, message_id)
    }
}
