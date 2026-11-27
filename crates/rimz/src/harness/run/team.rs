//! The team-long run an agent-launched team keeps on its leader.
//!
//! One run per stretch of work: opened by the launch prompt or by a flip out of
//! `Done`, fed the leader's final message at every turn end, and settled by
//! the flip into `Done` or the cohort's death, reporting the leader to the launcher
//! in a `TEAM_REPORT`.

use std::path::Path;

use crate::agents::{AgentDefinition, AgentState, PermissionMode};
use crate::config::DONE_STAGE;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::disk::summary::FileSummary;
use crate::ids::{MessageId, TeamInstanceId};
use crate::message::deliver::{DeliveryPolicy, deliver_one};
use crate::message::synthetic::SyntheticMessage;
use crate::store::message::{DeliveryGate, HarnessNotice, MessageSender};
use crate::store::run::{ReportTo, RunStatus, TeamRun};
use crate::workspace::ResolvedWorkspace;
use crate::{RuntimeScope, Store};

use super::report::{ReportErr, ResponseFile, compose_digest_row};
use super::{RecordMutation, Result, RunRecord};

/// Open the leader's team run for an agent-launched team's launch prompt.
pub(super) fn create_team_run(
    paths: &StatePaths,
    leader: &AgentState,
    reader: Option<&str>,
    adapter: &AgentDefinition,
    prompt: &str,
    cwd: &Path,
    report_to: ReportTo,
) -> Result<Option<RunRecord>> {
    if leader.launched_by.is_none() || prompt.trim().is_empty() || !super::peer_can_report(adapter)
    {
        return Ok(None);
    }
    let (Some(launch_id), Some(instance)) = (leader.launch_id.as_ref(), leader.team_instance())
    else {
        return Ok(None);
    };
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    if let Some(record) = open_team_run_for(paths, &instance)? {
        return Ok(Some(record));
    }
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        leader.kind.clone(),
        leader.mode.unwrap_or(PermissionMode::Auto),
        prompt.to_owned(),
        leader
            .worktree_path
            .as_deref()
            .map_or_else(|| cwd.to_path_buf(), Into::into),
    );
    record.agent_name = leader.name.clone();
    record.reader = reader.map(str::to_owned);
    record.report_to = report_to;
    record.team = Some(TeamRun {
        launch_id: launch_id.clone(),
        instance,
    });
    crate::store::run::write(&paths.runs_dir, &record)?;
    Ok(Some(record))
}

/// The open team run whose leader is `seat`'s launch.
pub fn open_team_run(paths: &StatePaths, seat: &AgentState) -> Result<Option<RunRecord>> {
    let launch_id = seat.launch_id.as_ref().unwrap_or(&seat.agent_id);
    Ok(super::open_listing(paths)?.into_iter().find(|record| {
        record
            .team
            .as_ref()
            .is_some_and(|team| &team.launch_id == launch_id)
    }))
}

/// The open team run of the cohort `instance`.
pub fn open_team_run_for(
    paths: &StatePaths,
    instance: &TeamInstanceId,
) -> Result<Option<RunRecord>> {
    Ok(team_runs(paths, instance)?
        .into_iter()
        .find(|record| !record.status.is_terminal()))
}

/// Settle the cohort's open run, stamping its report (absent when no launcher is left):
/// completed, or failed with `failure` as its tail.
/// Returns `None` when already settled so racing reporters cancel their own message.
pub fn settle_team_run(
    paths: &StatePaths,
    run_id: &crate::ids::RunId,
    report: Option<&MessageId>,
    failure: Option<&str>,
) -> Result<Option<RunRecord>> {
    let status = match failure {
        Some(_) => RunStatus::Failed,
        None => RunStatus::Completed,
    };
    let (record, settled) = super::update_record(paths, run_id, |record, now| {
        if !record.mark_terminal(status, now) {
            return Ok(RecordMutation::Keep(false));
        }
        record.report_message_id = report.cloned();
        if let Some(failure) = failure {
            record.failure_tail = Some(failure.into());
        }
        Ok(RecordMutation::Write(true))
    })?;
    Ok(settled.then_some(record))
}

/// Open a fresh team run for a board that left `Done`, carrying the task,
/// leader, and report policy of the cohort's newest run. `None` when the cohort never had one
/// (a user-launched team) or already has one open.
pub fn reopen_team_run(paths: &StatePaths, instance: &TeamInstanceId) -> Result<Option<RunRecord>> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let runs = team_runs(paths, instance)?;
    let Some(newest) = runs.first() else {
        return Ok(None);
    };
    if !newest.status.is_terminal() {
        return Ok(None);
    }
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        newest.kind.clone(),
        newest.permission_mode,
        newest.prompt.clone(),
        newest.worktree_path.clone(),
    );
    record.agent_id.clone_from(&newest.agent_id);
    record.agent_name.clone_from(&newest.agent_name);
    record.reader.clone_from(&newest.reader);
    record.transcript_path.clone_from(&newest.transcript_path);
    record.team.clone_from(&newest.team);
    record.report_to = newest.report_to;
    record.status = RunStatus::Running;
    crate::store::run::write(&paths.runs_dir, &record)?;
    Ok(Some(record))
}

/// The cohort's team runs, newest first.
fn team_runs(paths: &StatePaths, instance: &TeamInstanceId) -> Result<Vec<RunRecord>> {
    let mut runs = super::list(paths)?
        .into_iter()
        .filter(|record| {
            record
                .team
                .as_ref()
                .is_some_and(|team| &team.instance == instance)
        })
        .collect::<Vec<_>>();
    runs.sort_by_key(|run| std::cmp::Reverse(run.started_at));
    Ok(runs)
}

/// What a stage flip did for the team's launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamReportOutcome {
    /// A `TEAM_REPORT` is queued for `launcher`, and `delivered` when it went out now.
    Queued { launcher: String, delivered: bool },
    /// The board left `Done`; the leader's next stretch of work reports at the next `Done`.
    Reopened,
    /// A detached team settled or reopened: its run follows the board and tells nobody.
    Detached,
    /// No agent launched this cohort, or the flip does not touch `Done`.
    NotOwed,
}

/// Settle the cohort's team report for one stage flip of `instance`: a flip into `Done` owes
/// the launcher the `TEAM_REPORT` of the run still open, from any stage, so the retry of a flip
/// into `Done` whose report failed sends it; and a flip out of `Done` reopens the run so the next
/// `Done` reports again.
pub(in crate::harness) fn team_report_on_flip(
    workspace: &ResolvedWorkspace,
    store: &Store,
    instance: &TeamInstanceId,
    board: &Path,
    from: Option<&str>,
    to: &str,
) -> std::result::Result<TeamReportOutcome, ReportErr> {
    if to == DONE_STAGE {
        return report_done(workspace, store, instance, board);
    }
    if from == Some(DONE_STAGE)
        && let Some(reopened) = reopen_team_run(store.paths(), instance)?
    {
        return Ok(match reopened.report_to {
            ReportTo::Launcher => TeamReportOutcome::Reopened,
            ReportTo::Nobody => TeamReportOutcome::Detached,
        });
    }
    Ok(TeamReportOutcome::NotOwed)
}

fn report_done(
    workspace: &ResolvedWorkspace,
    store: &Store,
    instance: &TeamInstanceId,
    board: &Path,
) -> std::result::Result<TeamReportOutcome, ReportErr> {
    let Some(record) = open_team_run_for(store.paths(), instance)? else {
        return Ok(TeamReportOutcome::NotOwed);
    };
    report_team_run(workspace, store, &record, board, Some(DONE_STAGE))
}

const COHORT_ENDED: &str = "cohort ended before Done";

/// Report the cohort's settled team run to its launcher, the leader's answer as its one row:
/// at `Done` when `stage` is `Done`, else as a cohort that ended before it.
pub fn report_team_run(
    workspace: &ResolvedWorkspace,
    store: &Store,
    record: &RunRecord,
    board: &Path,
    stage: Option<&str>,
) -> std::result::Result<TeamReportOutcome, ReportErr> {
    let failure = (stage != Some(DONE_STAGE)).then_some(COHORT_ENDED);
    // Constructed with a team marker by `open_team_run_for`'s filter.
    let Some(team) = record.team.as_ref() else {
        return Ok(TeamReportOutcome::NotOwed);
    };
    if record.report_to == ReportTo::Nobody {
        // Detached: the run follows the board and leaves the answer on disk for a join.
        super::publish_response(store.paths(), record)?;
        settle_team_run(store.paths(), &record.run_id, None, failure)?;
        return Ok(TeamReportOutcome::Detached);
    }
    let instance = &team.instance;
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let agents = &projection.agents;
    let leader = crate::address::launch_row(agents, &record.kind, &team.launch_id);
    let launcher = leader
        .and_then(|leader| leader.launcher())
        .and_then(|(kind, id)| crate::address::launch_row(agents, kind, id))
        .filter(|launcher| launcher.ended_at.is_none());
    let (Some(leader), Some(launcher)) = (leader, launcher) else {
        // Nobody to tell: settle the run so the next Done does not report it.
        settle_team_run(store.paths(), &record.run_id, None, failure)?;
        return Ok(TeamReportOutcome::NotOwed);
    };

    let published = super::publish_response(store.paths(), record)?;
    let response = match published {
        Some(path) => Some(ResponseFile {
            summary: FileSummary::measure(&path).map_err(|source| ReportErr::Response {
                path: path.clone(),
                source,
            })?,
            path,
        }),
        None => None,
    };
    let settled = RunRecord {
        status: if failure.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        },
        failure_tail: failure.map(Into::into),
        completed_at: Some(jiff::Timestamp::now()),
        ..record.clone()
    };
    let outcome = match failure {
        None => "reached Done".to_owned(),
        Some(_) => format!("ended before Done at stage {}", stage.unwrap_or("no board")),
    };
    let text = format!(
        "Team {instance} {outcome}; its leader reports:\n{}\nMemory: {}",
        compose_digest_row(leader, &settled, response.as_ref()),
        board.display()
    );
    let pane_id = launcher.pane.as_ref().map(|pane| &pane.pane_id);
    let message = SyntheticMessage {
        agent: launcher,
        text,
        sender: MessageSender::Harness {
            notice: HarnessNotice::TeamReport,
        },
        gate: DeliveryGate::Done,
        premises: crate::store::message::Premise::NONE,
    }
    .record(workspace);
    let message_id: MessageId = message.message_id.clone();
    // Queue before settling: a crash in between re-reports at the next Done
    // rather than losing this one.
    store.queue_message(&message, &workspace.session_name)?;
    if settle_team_run(store.paths(), &record.run_id, Some(&message_id), failure)?.is_none() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A cohort `forge#x` led by `lead`, launched by the live agent `boss`, with its open team run.
    fn launched_team(
        report_to: ReportTo,
    ) -> (tempfile::TempDir, ResolvedWorkspace, Store, RunRecord) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id: crate::WorkspaceId::from_project_root(dir.path()),
            project_root: dir.path().into(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: dir.path().into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        };
        let store = Store::open(
            crate::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap(),
            crate::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt"))
                .unwrap(),
        )
        .unwrap();
        let kind = crate::ids::AgentKind::new_unchecked("codex");
        for name in ["boss", "lead"] {
            let mut observation = crate::agents::AgentLifecycleObservation::new(
                Some(name.into()),
                crate::agents::LifecycleSignal::Registered,
            );
            observation.agent_name = Some(name.into());
            if name == "lead" {
                observation.launch.team = Some("forge".into());
                observation.launch.channel = Some("x".into());
                observation.launch.launched_by = Some(Box::new(crate::agents::LaunchedBy {
                    kind: kind.clone(),
                    agent_id: "boss".into(),
                }));
            }
            store
                .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
                    session_name: "room",
                    agent_kind: kind.clone(),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        let mut record = RunRecord::new(
            workspace.workspace_id.clone(),
            kind,
            crate::agents::PermissionMode::Auto,
            "work".into(),
            dir.path().into(),
        );
        record.team = Some(crate::store::run::TeamRun {
            launch_id: "lead".into(),
            instance: "forge#x".parse().unwrap(),
        });
        record.agent_name = Some("lead".into());
        record.last_message = Some("shipped".into());
        record.report_to = report_to;
        crate::harness::run::create(store.paths(), &record).unwrap();
        (dir, workspace, store, record)
    }

    #[test]
    fn a_detached_team_settles_and_reopens_without_telling_its_launcher() {
        for stage in [Some(DONE_STAGE), None] {
            let (dir, workspace, store, record) = launched_team(ReportTo::Nobody);
            let board = dir.path().join("blackboard.md");
            assert_eq!(
                report_team_run(&workspace, &store, &record, &board, stage).unwrap(),
                TeamReportOutcome::Detached
            );
            let settled = crate::harness::run::load(store.paths(), &record.run_id).unwrap();
            assert_eq!(
                settled.status,
                if stage.is_some() {
                    RunStatus::Completed
                } else {
                    RunStatus::Failed
                }
            );
            assert_eq!(settled.report_message_id, None);
            assert!(store.list_messages().unwrap().is_empty());
            assert!(store.list_message_history().unwrap().is_empty());
            let response = crate::harness::run::response_path(store.paths(), &settled).unwrap();
            assert_eq!(std::fs::read_to_string(response).unwrap(), "shipped\n");

            assert_eq!(
                team_report_on_flip(
                    &workspace,
                    &store,
                    &"forge#x".parse().unwrap(),
                    &board,
                    Some(DONE_STAGE),
                    "Build"
                )
                .unwrap(),
                TeamReportOutcome::Detached
            );
            let reopened = open_team_run_for(store.paths(), &"forge#x".parse().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(reopened.report_to, ReportTo::Nobody);
            assert!(store.list_messages().unwrap().is_empty());
        }
    }

    #[test]
    fn competing_done_and_death_reports_cancel_the_loser_in_either_order() {
        for first_stage in [Some(DONE_STAGE), None] {
            let (dir, workspace, store, record) = launched_team(ReportTo::Launcher);
            let board = dir.path().join("blackboard.md");
            assert!(matches!(
                report_team_run(&workspace, &store, &record, &board, first_stage).unwrap(),
                TeamReportOutcome::Queued { .. }
            ));
            let second_stage = if first_stage.is_some() {
                None
            } else {
                Some(DONE_STAGE)
            };
            assert_eq!(
                report_team_run(&workspace, &store, &record, &board, second_stage).unwrap(),
                TeamReportOutcome::NotOwed
            );
            let messages = store.list_messages().unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| !message.status.is_terminal())
                    .count(),
                1
            );
            let history = store.list_message_history().unwrap();
            assert_eq!(history.len(), 1);
            let canceled = history
                .iter()
                .find(|message| message.status == crate::store::message::MessageStatus::Canceled)
                .unwrap();
            assert!(store.read_events().unwrap().iter().any(|event| matches!(
                event.kind(),
                crate::store::event::EventKind::Message { payload, .. }
                    if payload.message_id == canceled.message_id
                        && payload.reason.as_deref() == Some("team run already settled")
            )));
            let settled = crate::harness::run::load(store.paths(), &record.run_id).unwrap();
            assert_eq!(
                settled.status,
                if first_stage.is_some() {
                    RunStatus::Completed
                } else {
                    RunStatus::Failed
                }
            );
            if first_stage.is_none() {
                assert!(
                    messages.iter().any(|message| message
                        .text
                        .contains("ended before Done at stage no board"))
                );
            }
            for from in [Some("Build"), Some(DONE_STAGE)] {
                assert_eq!(
                    team_report_on_flip(
                        &workspace,
                        &store,
                        &"forge#x".parse().unwrap(),
                        &board,
                        from,
                        DONE_STAGE
                    )
                    .unwrap(),
                    TeamReportOutcome::NotOwed
                );
            }
            assert_eq!(store.list_messages().unwrap().len(), 1);
            assert_eq!(store.list_message_history().unwrap().len(), 1);
        }
    }
}
