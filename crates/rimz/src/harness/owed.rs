//! Harness wakes still owed to a supervised agent session.

use crate::ids::{AgentKind, AgentSessionId};
use crate::message::reply::TurnWaitView;
use crate::store::StoreErr;
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
    if let Some(launcher) = projection
        .agents
        .iter()
        .find(|agent| &agent.kind == kind && &agent.agent_id == agent_id)
        && super::fleet::has_members(&projection.agents, launcher)
    {
        let runs = super::run::list(store.paths())?;
        let fleet = FleetRuns::of(&projection.agents, &runs, launcher);
        if fleet.any_running() || !fleet.unreported().is_empty() {
            return Ok(Some(OwedWake::Subagents));
        }
    }
    let view = TurnWaitView::load(store)?;
    let Some(agent) = view
        .snapshot
        .agents
        .iter()
        .find(|agent| &agent.kind == kind && &agent.agent_id == agent_id)
    else {
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
}

impl OwedWake {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::WakeInFlight => "wake in flight",
            Self::Subagents => "subagents",
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
    use crate::store::run::{RunRecord, RunStatus};
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
    fn owed_messages_hold_only_their_card_until_terminal() {
        for notice in [
            HarnessNotice::Wait,
            HarnessNotice::Signal,
            HarnessNotice::SubagentReport,
            HarnessNotice::Stage,
            HarnessNotice::Deadline,
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
                    && !matches!(notice, HarnessNotice::Stage | HarnessNotice::Deadline))
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
    fn owed_fleet_includes_ended_unreported_children() {
        for (status, joined, reported, expected) in [
            (RunStatus::Running, false, false, Some(OwedWake::Subagents)),
            (
                RunStatus::Completed,
                false,
                false,
                Some(OwedWake::Subagents),
            ),
            (RunStatus::Completed, true, false, None),
            (RunStatus::Completed, false, true, None),
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
                "{status:?} joined={joined} reported={reported}"
            );
        }
    }
}
