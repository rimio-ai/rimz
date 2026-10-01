//! The `TEAM_REPORT` an agent-launched team owes its launcher at `Done` or cohort death.
//!
//! The leader's team run collects its final message across turns. The flip
//! into `Done` publishes that message, queues one report to the launcher, and
//! settles the run; a flip back out opens a fresh run, so every `Done` reports.

use std::path::Path;

use rimz::config::DONE_STAGE;
use rimz::disk::summary::FileSummary;
use rimz::harness::run;
use rimz::ids::{AgentSessionId, MessageId};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};
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
    board: &Path,
    from: Option<&str>,
    to: &str,
) -> anyhow::Result<TeamReportOutcome> {
    let was_done = from == Some(DONE_STAGE);
    if to == DONE_STAGE && !was_done {
        return Ok(report_done(workspace, store, instance, board)?);
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
    board: &Path,
) -> Result<TeamReportOutcome, ReportErr> {
    let Some(record) = run::open_team_run_for(store.paths(), instance)? else {
        return Ok(TeamReportOutcome::NotOwed);
    };
    report_team(workspace, store, &record, board, Some(DONE_STAGE))
}

const COHORT_ENDED: &str = "cohort ended before Done";

pub(super) fn settle_ended_teams(
    workspace: &ResolvedWorkspace,
    store: &Store,
    launcher_id: &AgentSessionId,
) -> Result<(), ReportErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(launcher) =
        super::subagent_report::report_parent(&projection.agents, launcher_id, None)
            .filter(|launcher| launcher.ended_at.is_none())
    else {
        return Ok(());
    };
    // A parked run asks this on every strand poll: skip the runs read unless
    // this launcher launched a team.
    if !projection
        .agents
        .iter()
        .any(|agent| agent.is_team_seat() && agent.launcher_is(launcher))
    {
        return Ok(());
    }
    let runs = run::list(store.paths())?;
    for record in rimz::harness::fleet::ended_team_runs(&projection.agents, &runs, launcher) {
        let stage = rimz::harness::scratch::board_stage(&record.worktree_path);
        let board = record.worktree_path.join(rimz::harness::board::BOARD_FILE);
        if let Err(error) = report_team(
            workspace,
            store,
            record,
            &board,
            stage.as_ref().map(|stage| stage.name.as_str()),
        ) {
            tracing::debug!(run_id = %record.run_id, %error, "could not settle an ended team run");
        }
    }
    Ok(())
}

fn report_team(
    workspace: &ResolvedWorkspace,
    store: &Store,
    record: &RunRecord,
    board: &Path,
    stage: Option<&str>,
) -> Result<TeamReportOutcome, ReportErr> {
    let failure = (stage != Some(DONE_STAGE)).then_some(COHORT_ENDED);
    // Constructed with a team marker by `open_team_run_for`'s filter.
    let Some(team) = record.team.as_ref() else {
        return Ok(TeamReportOutcome::NotOwed);
    };
    let instance = &team.instance;
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let agents = &projection.agents;
    let leader = rimz::address::launch_row(agents, &record.kind, &team.launch_id);
    let launcher = leader
        .and_then(|leader| leader.launcher())
        .and_then(|(kind, id)| rimz::address::launch_row(agents, kind, id))
        .filter(|launcher| launcher.ended_at.is_none());
    let (Some(leader), Some(launcher)) = (leader, launcher) else {
        // Nobody to tell: settle the run so the next Done does not report it.
        run::settle_team_run(store.paths(), &record.run_id, None, failure)?;
        return Ok(TeamReportOutcome::NotOwed);
    };

    let published = run::publish_response(store.paths(), record)?;
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
        pane_id: pane_id.cloned(),
    }
    .record(workspace);
    let message_id: MessageId = message.message_id.clone();
    // Queue before settling: a crash in between re-reports at the next Done
    // rather than losing this one.
    store.queue_message(&message, &workspace.session_name)?;
    if run::settle_team_run(store.paths(), &record.run_id, Some(&message_id), failure)?.is_none() {
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

    #[test]
    fn competing_done_and_death_reports_cancel_the_loser_in_either_order() {
        for first_stage in [Some(DONE_STAGE), None] {
            let dir = tempfile::tempdir().unwrap();
            let workspace = ResolvedWorkspace {
                workspace_id: rimz::WorkspaceId::from_project_root(dir.path()),
                project_root: dir.path().into(),
                cwd_project_root: None,
                root_class: rimz::workspace::RootClass::Directory,
                worktree_root: dir.path().into(),
                worktree_branch: None,
                session_name: "room".into(),
                mux_hint: None,
            };
            let store = Store::open(
                rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap(),
                rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt"))
                    .unwrap(),
            )
            .unwrap();
            let kind = rimz::ids::AgentKind::new_unchecked("codex");
            for name in ["boss", "lead"] {
                let mut observation = rimz::agents::AgentLifecycleObservation::new(
                    Some(name.into()),
                    rimz::agents::LifecycleSignal::Registered,
                );
                observation.agent_name = Some(name.into());
                if name == "lead" {
                    observation.launch.team = Some("forge".into());
                    observation.launch.channel = Some("x".into());
                    observation.launch.launched_by = Some(Box::new(rimz::agents::LaunchedBy {
                        kind: kind.clone(),
                        agent_id: "boss".into(),
                    }));
                }
                store
                    .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
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
                rimz::agents::PermissionMode::Auto,
                "work".into(),
                dir.path().into(),
            );
            record.team = Some(rimz::store::run::TeamRun {
                launch_id: "lead".into(),
                instance: "forge#x".into(),
            });
            record.agent_name = Some("lead".into());
            run::create(store.paths(), &record).unwrap();
            let board = dir.path().join("blackboard.md");
            assert!(matches!(
                report_team(&workspace, &store, &record, &board, first_stage).unwrap(),
                TeamReportOutcome::Queued { .. }
            ));
            let second_stage = if first_stage.is_some() {
                None
            } else {
                Some(DONE_STAGE)
            };
            assert_eq!(
                report_team(&workspace, &store, &record, &board, second_stage).unwrap(),
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
                .find(|message| message.status == rimz::store::message::MessageStatus::Canceled)
                .unwrap();
            assert!(store.read_events().unwrap().iter().any(|event| matches!(
                event.kind(),
                rimz::store::event::EventKind::Message { payload, .. }
                    if payload.message_id == canceled.message_id
                        && payload.reason.as_deref() == Some("team run already settled")
            )));
            let settled = run::load(store.paths(), &record.run_id).unwrap();
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
            assert_eq!(
                on_flip(
                    &workspace,
                    &store,
                    "forge#x",
                    &board,
                    Some("Build"),
                    DONE_STAGE
                )
                .unwrap(),
                TeamReportOutcome::NotOwed
            );
            assert_eq!(store.list_messages().unwrap().len(), 1);
            assert_eq!(store.list_message_history().unwrap().len(), 1);
        }
    }
}
