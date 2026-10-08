//! Harness wakes still owed to a supervised agent session.

use crate::ids::{AgentKind, AgentSessionId};
use crate::message::reply::TurnWaitView;
use crate::store::StoreErr;
use crate::store::snapshot::find_agent;
use crate::{RuntimeScope, Store};

use super::fleet::FleetRuns;

/// Whether this session is still owed a harness wake. Each term is read
/// before the writes that would hide it.
///
/// The fleet term goes first: the digest reporter stamps `report_message_id`
/// on the child rows and only then queues the digest, so a reader that finds
/// the rows unstamped is owed, and one that finds them stamped reads the queue
/// afterwards, where the digest by then is. Reading the queue first would
/// widen that window by this reader's own projection and runs-directory time.
///
/// The team reporter queues before stamping the run: an open run is owed
/// (unless its board already reads Done), and a settled run's report is in
/// the later queue read. That read must therefore count `TeamReport` too.
///
/// The wait terms keep `TurnWaitView`'s order, catalog → queue → rollup: a
/// wake publishes its message record before it consumes its catalog row, and a
/// provider's turn start lands before the delivery ack settles that record, so
/// every wake in flight shows in at least one of the three reads.
///
/// Accepted gap, now bounded by the reporter's own two lock holds: it stamps
/// and queues under separate locks, so a parent Stop landing between them can
/// complete with a digest about to be queued. Keep that tested durability
/// sequence; the stranded-park settle closes its half with a second look.
pub fn owed_wake(
    store: &Store,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<Option<OwedWake>, StoreErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    if let Some(launcher) = find_agent(&projection.agents, kind, agent_id) {
        let has_members = super::fleet::has_members(&projection.agents, launcher);
        let launched_team = projection
            .agents
            .iter()
            .any(|agent| agent.is_team_seat() && agent.launcher_is(launcher));
        if has_members || launched_team {
            let runs = super::run::list(store.paths())?;
            if has_members {
                let fleet = FleetRuns::of(&projection.agents, &runs, launcher);
                if fleet.any_running() || !fleet.unreported().is_empty() {
                    return Ok(Some(OwedWake::Subagents));
                }
            }
            if launched_team
                && super::fleet::owed_team_runs(&projection.agents, &runs, launcher)
                    .iter()
                    .any(|run| {
                        let stage = super::scratch::board_stage(&run.worktree_path);
                        super::fleet::team_stage_pending(
                            stage.as_ref().map(|stage| stage.name.as_str()),
                        )
                    })
            {
                return Ok(Some(OwedWake::Team));
            }
        }
    }
    let view = TurnWaitView::load(store)?;
    let Some(agent) = find_agent(&view.snapshot.agents, kind, agent_id) else {
        return Ok(None);
    };
    if !agent.pending_waits.is_empty() {
        return Ok(Some(OwedWake::Wait));
    }
    Ok(view
        .wake_in_flight(agent, true)
        .then_some(OwedWake::WakeInFlight))
}

/// What still owes an agent session a harness wake, as the run fold sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwedWake {
    Wait,
    WakeInFlight,
    Subagents,
    Team,
}

impl OwedWake {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::WakeInFlight => "wake in flight",
            Self::Subagents => "subagents",
            Self::Team => "team",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentLifecycleObservation, LifecycleSignal, PermissionMode};
    use crate::store::message::{
        DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
    };
    use crate::store::run::{ReportTo, RunRecord, RunStatus};
    use crate::store::writer::AgentLifecycleIntent;
    use crate::{RuntimePaths, StatePaths, WorkspaceId};

    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(id.clone(), &dir.path().join("state")).unwrap();
        let runtime = RuntimePaths::under(id, &dir.path().join("runtime")).unwrap();
        let store = Store::open(state, runtime).unwrap();
        (dir, store)
    }

    fn register(store: &Store, name: &str, parent: Option<&str>) {
        let mut observation =
            AgentLifecycleObservation::new(Some(name.into()), LifecycleSignal::Registered);
        observation.agent_name = Some(name.to_owned());
        observation.pane_id = Some(
            crate::ids::PaneId::parse(match name {
                "parent" => "tmux:%1",
                "child" => "tmux:%2",
                "other" => "tmux:%3",
                _ => unreachable!("fixture names have distinct panes"),
            })
            .unwrap(),
        );
        if let Some(parent) = parent {
            observation.launch.parent_agent_id = Some(parent.into());
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
            observation.launch.launch_depth = Some(1);
        }
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "owed-test",
                agent_kind: AgentKind::new_unchecked("codex"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }

    #[test]
    fn owed_team_is_scoped_to_its_launcher_and_skips_done_boards() {
        for (own, status, done, detached, expected) in [
            (true, RunStatus::Running, false, false, Some(OwedWake::Team)),
            (true, RunStatus::Completed, false, false, None),
            (false, RunStatus::Running, false, false, None),
            (true, RunStatus::Running, true, false, None),
            (true, RunStatus::Running, false, true, None),
        ] {
            let (dir, store) = fixture();
            register(&store, "parent", None);
            let kind = AgentKind::new_unchecked("codex");
            let mut observation =
                AgentLifecycleObservation::new(Some("leader".into()), LifecycleSignal::Registered);
            observation.agent_name = Some("leader".into());
            observation.launch.team = Some("forge".into());
            observation.launch.launched_by = Some(Box::new(crate::agents::LaunchedBy {
                kind: kind.clone(),
                agent_id: if own { "parent" } else { "someone-else" }.into(),
            }));
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "owed-test",
                    agent_kind: kind.clone(),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
            let mut record = RunRecord::new(
                store.paths().workspace_id.clone(),
                kind.clone(),
                PermissionMode::Auto,
                "work".into(),
                dir.path().into(),
            );
            record.status = status;
            if detached {
                record.report_to = ReportTo::Nobody;
            }
            record.team = Some(crate::store::run::TeamRun {
                launch_id: "leader".into(),
                instance: "forge#external".into(),
            });
            super::super::run::create(store.paths(), &record).unwrap();
            if done {
                std::fs::write(dir.path().join("blackboard.md"), "Stage: Done\n").unwrap();
            }
            assert_eq!(OwedWake::Team.as_str(), "team");
            assert_eq!(
                owed_wake(&store, &kind, &"parent".into()).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn owed_messages_hold_only_their_card_until_terminal() {
        for notice in [
            HarnessNotice::Wait,
            HarnessNotice::Signal,
            HarnessNotice::SubagentReport,
            HarnessNotice::TeamReport,
            HarnessNotice::Stage,
            HarnessNotice::Deadline,
            HarnessNotice::SubagentPaused,
            HarnessNotice::SubagentStalled,
        ] {
            for status in [
                MessageStatus::Queued,
                MessageStatus::Claimed,
                MessageStatus::Sent,
                MessageStatus::Delivered,
                MessageStatus::TimedOut,
            ] {
                let (_dir, store) = fixture();
                register(&store, "parent", None);
                register(&store, "other", None);
                let agent = store
                    .snapshot_cached()
                    .unwrap()
                    .agents
                    .into_iter()
                    .find(|agent| agent.agent_id.as_str() == "parent")
                    .unwrap();
                let mut message = MessageRecord::new(
                    store.paths().workspace_id.clone(),
                    &agent,
                    "wake".to_owned(),
                    DeliveryGate::Done,
                )
                .with_sender(MessageSender::Harness {
                    notice: notice.clone(),
                });
                message.status = status;
                store.queue_message(&message, "owed-test").unwrap();
                let expected = (!status.is_terminal()
                    && !matches!(
                        notice,
                        HarnessNotice::Stage
                            | HarnessNotice::Deadline
                            | HarnessNotice::SubagentPaused
                            | HarnessNotice::SubagentStalled
                    ))
                .then_some(OwedWake::WakeInFlight);
                assert_eq!(
                    owed_wake(&store, &agent.kind, &agent.agent_id).unwrap(),
                    expected,
                    "{notice:?} {status:?}"
                );
                for id in ["other", "missing"] {
                    assert_eq!(owed_wake(&store, &agent.kind, &id.into()).unwrap(), None);
                }
                assert_eq!(
                    owed_wake(&store, &AgentKind::new_unchecked("claude"), &agent.agent_id)
                        .unwrap(),
                    None
                );
            }
        }
    }

    #[test]
    fn overdue_sent_wake_is_not_owed_without_reconciliation() {
        let (_dir, store) = fixture();
        register(&store, "parent", None);
        let agent = store
            .snapshot_cached()
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.agent_id.as_str() == "parent")
            .unwrap();
        let now = jiff::Timestamp::now();
        let mut message = MessageRecord::new(
            store.paths().workspace_id.clone(),
            &agent,
            "wake".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::Harness {
            notice: HarnessNotice::Wait,
        });
        message.status = MessageStatus::Sent;
        let overdue = now - message.body.delivery_window() - std::time::Duration::from_secs(60);
        for (sent_at, retry_after, expected) in [
            (overdue, None, None),
            (now, None, Some(OwedWake::WakeInFlight)),
            (
                overdue,
                Some(now + message.body.delivery_window()),
                Some(OwedWake::WakeInFlight),
            ),
        ] {
            message.updated_at = sent_at;
            message.last_sent_at = Some(sent_at);
            message.retry_after = retry_after;
            store.queue_message(&message, "owed-test").unwrap();
            assert_eq!(
                owed_wake(&store, &agent.kind, &agent.agent_id).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn owed_fleet_includes_ended_unreported_children() {
        for (status, joined, reported, report_to, expected) in [
            (
                RunStatus::Running,
                false,
                false,
                ReportTo::Launcher,
                Some(OwedWake::Subagents),
            ),
            (
                RunStatus::Completed,
                false,
                false,
                ReportTo::Launcher,
                Some(OwedWake::Subagents),
            ),
            (RunStatus::Completed, true, false, ReportTo::Launcher, None),
            (RunStatus::Completed, false, true, ReportTo::Launcher, None),
            (RunStatus::Running, false, false, ReportTo::Nobody, None),
            (RunStatus::Completed, false, false, ReportTo::Nobody, None),
        ] {
            let (dir, store) = fixture();
            register(&store, "parent", None);
            register(&store, "child", Some("parent"));
            let kind = AgentKind::new_unchecked("codex");
            let mut run = RunRecord::new(
                store.paths().workspace_id.clone(),
                kind.clone(),
                PermissionMode::Auto,
                "work".to_owned(),
                dir.path().to_owned(),
            );
            run.agent_id = Some("child".into());
            run.status = status;
            run.report_to = report_to;
            run.joined_at = joined.then_some(jiff::Timestamp::now());
            run.report_message_id = reported.then(crate::MessageId::new);
            super::super::run::create(store.paths(), &run).unwrap();
            if status.is_terminal() {
                let observation =
                    AgentLifecycleObservation::new(Some("child".into()), LifecycleSignal::Ended);
                store
                    .append_agent_lifecycle(AgentLifecycleIntent {
                        session_name: "owed-test",
                        agent_kind: kind.clone(),
                        event_name: "test",
                        observation: &observation,
                        spawned_subagents: &[],
                    })
                    .unwrap();
            }
            assert_eq!(
                owed_wake(&store, &kind, &"parent".into()).unwrap(),
                expected,
                "{status:?} joined={joined} reported={reported} {report_to:?}"
            );
        }
    }
}
