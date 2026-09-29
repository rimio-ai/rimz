//! Durable membership markers for parent-owned results and subagent report digests.
//!
//! An answer printed during the caller's open turn (or to a human shell), or
//! dismissed by the parent's `rimz subagents stop`, is
//! excluded from a digest that has not been composed. Once a digest exists, it
//! may be canceled only after every listed answer has been claimed this way,
//! preserving the notice while any row remains unread.

use crate::disk::paths::StatePaths;
use crate::ids::{MessageId, RunId};
use crate::store::Store;

use super::{RecordMutation, Result, RunRecord, update_record};

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

    use crate::agents::PermissionMode;
    use crate::ids::{AgentKind, WorkspaceId};

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
}
