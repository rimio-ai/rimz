//! Durable membership markers for parent-owned results and subagent report digests.
//!
//! An answer printed during the caller's open turn (or to a human shell), or
//! dismissed by the parent's `rimz subagents stop`, is
//! excluded from a digest that has not been composed. Once a digest exists, it
//! may be canceled only after every listed answer has been claimed this way,
//! preserving the notice while any row remains unread.

use crate::disk::paths::StatePaths;
use crate::harness::ancestry::{CallerIdentity, resolve_launch_caller};
use crate::ids::{MessageId, RunId};
use crate::store::Store;
use std::time::{Duration, Instant};

use super::{RecordMutation, Result, RunRecord, list, update_record};

/// Settle ceiling for a reply claim: the reply is read from the card, and the
/// run fold that settles its answer lands in the same hook, moments apart.
const REPLY_SETTLE_CEILING: Duration = Duration::from_secs(2);

/// Join the settled answer a printed `rimz message --wait` reply came from.
///
/// Claims only for the child's attended launcher or a human shell (`caller`
/// None), and only an answer whose `opened_by` holds `message_id`. Returns at
/// once when no such answer exists; waits for one to settle until `deadline`
/// or the settle ceiling, then leaves it to the digest.
pub fn claim_reply(
    store: &Store,
    session_name: &str,
    message_id: &MessageId,
    caller: Option<&CallerIdentity>,
    deadline: Option<Instant>,
) -> crate::store::Result<()> {
    let ceiling = Instant::now() + REPLY_SETTLE_CEILING;
    let deadline = deadline.map_or(ceiling, |deadline| deadline.min(ceiling));
    loop {
        let snapshot = store.snapshot_cached()?;
        let parent = match caller {
            None => None,
            Some(caller) => match resolve_launch_caller(&snapshot.agents, caller) {
                Ok(parent) if parent.holds_open_turn() => Some(parent),
                _ => return Ok(()),
            },
        };
        let mut unsettled = false;
        for record in list(store.paths())? {
            if record.team.is_some() || (!record.subagent && record.peer.is_none()) {
                continue;
            }
            if parent.is_some_and(|parent| {
                !snapshot
                    .agents
                    .iter()
                    .any(|child| record.matches_agent(child) && child.launcher_is(parent))
            }) {
                continue;
            }
            match answer_opened_by(&record, message_id) {
                Some((ordinal, true)) => {
                    return join_and_settle_digest(
                        store,
                        session_name,
                        &record.run_id,
                        Some(ordinal),
                        "reply printed inline",
                    );
                }
                Some((_, false)) => unsettled = true,
                None => {}
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if !unsettled || left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(Duration::from_millis(25)));
    }
}

/// The answer `message_id` opened, as its ordinal and whether it has settled.
fn answer_opened_by(record: &RunRecord, message_id: &MessageId) -> Option<(u32, bool)> {
    let opened_by = record
        .peer
        .as_ref()
        .map_or(&record.opened_by, |peer| &peer.opened_by);
    if opened_by.contains(message_id) {
        return Some((record.follow_ups + 1, record.status.is_terminal()));
    }
    record
        .earlier_answers
        .iter()
        .find(|answer| answer.opened_by.contains(message_id))
        .map(|answer| (answer.ordinal, answer.status.is_terminal()))
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
            store.cancel_message(&message_id, session_name, reason)?;
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
    fn presented_reply_claims_only_its_settled_answer_for_attended_owner() {
        use crate::harness::run;
        use crate::store::run::{EarlierAnswer, PeerRun};

        for answer in ["current", "earlier", "peer"] {
            for caller_kind in ["parent", "human", "stranger", "unattended", "missing"] {
                let (_dir, store, mut record, message_id) = claim_fixture();
                if answer == "earlier" {
                    record.earlier_answers.push(EarlierAnswer {
                        ordinal: 1,
                        status: RunStatus::Completed,
                        started_at: record.started_at,
                        completed_at: record.completed_at,
                        prompt: None,
                        failure_tail: None,
                        opened_by: std::mem::take(&mut record.opened_by),
                        report_message_id: None,
                        joined_at: None,
                    });
                    record.follow_ups = 1;
                    record.status = RunStatus::Running;
                } else if answer == "peer" {
                    record.subagent = false;
                    record.peer = Some(PeerRun {
                        launch_id: "child".into(),
                        opened_by: std::mem::take(&mut record.opened_by),
                    });
                }
                run::create(store.paths(), &record).unwrap();
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
                let caller = (caller_kind != "human").then(|| CallerIdentity {
                    kind: AgentKind::new_unchecked("claude"),
                    launch_id: Some(caller_name.into()),
                    pane_id: None,
                    name: Some(caller_name.into()),
                    profile: None,
                    role: None,
                });
                claim_reply(&store, "test", &message_id, caller.as_ref(), None).unwrap();
                let claimed = run::load(store.paths(), &record.run_id).unwrap();
                let joined = if answer == "earlier" {
                    assert!(claimed.joined_at.is_none());
                    claimed.earlier_answers[0].joined_at
                } else {
                    claimed.joined_at
                };
                assert_eq!(
                    joined.is_some(),
                    matches!(caller_kind, "parent" | "human"),
                    "{answer}/{caller_kind}"
                );
                if answer != "earlier" && joined.is_some() {
                    assert!(!claimed.owes_report());
                }
            }
        }
    }

    #[test]
    fn presented_reply_without_an_opened_answer_returns_at_once() {
        let (_dir, store, record, _) = claim_fixture();
        let started = Instant::now();
        claim_reply(&store, "test", &MessageId::new(), None, None).unwrap();
        assert!(started.elapsed() < REPLY_SETTLE_CEILING / 4);
        assert!(
            super::super::load(store.paths(), &record.run_id)
                .unwrap()
                .joined_at
                .is_none()
        );
    }

    #[test]
    fn presented_reply_waits_for_durable_settlement_but_not_past_deadline() {
        use crate::harness::run;

        let (_dir, store, mut record, message_id) = claim_fixture();
        record.status = RunStatus::Running;
        run::create(store.paths(), &record).unwrap();
        claim_reply(&store, "test", &message_id, None, Some(Instant::now())).unwrap();
        assert!(
            run::load(store.paths(), &record.run_id)
                .unwrap()
                .joined_at
                .is_none()
        );
        let settling_store = store.clone();
        let settling_id = record.run_id.clone();
        let settling = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut settled = run::load(settling_store.paths(), &settling_id).unwrap();
            settled.status = RunStatus::Completed;
            run::create(settling_store.paths(), &settled).unwrap();
        });
        claim_reply(
            &store,
            "test",
            &message_id,
            None,
            Some(Instant::now() + Duration::from_secs(1)),
        )
        .unwrap();
        settling.join().unwrap();
        assert!(
            run::load(store.paths(), &record.run_id)
                .unwrap()
                .joined_at
                .is_some()
        );
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
