//! Durable membership markers for inline joins and subagent report digests.
//!
//! A joined run is excluded from a digest that has not been composed. Once a
//! digest exists, the joiner may cancel it only after every listed run has been
//! joined, preserving the notice while any row remains unread.

use crate::ids::{MessageId, RunId};
use crate::store::StatePaths;

use super::{RecordMutation, Result, RunRecord, update_record};

pub fn mark_joined(paths: &StatePaths, run_id: &RunId) -> Result<(RunRecord, bool)> {
    let record = update_record(paths, run_id, |record, now| {
        if record.joined_at.is_some() {
            return Ok(RecordMutation::Keep(()));
        }
        record.joined_at = Some(now);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)?;
    let fully_joined = match record.report_message_id.as_ref() {
        Some(message_id) => digest_fully_joined(paths, message_id)?,
        None => false,
    };
    Ok((record, fully_joined))
}

pub fn record_report_message(
    paths: &StatePaths,
    run_id: &RunId,
    message_id: MessageId,
) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        if record.report_message_id.is_some() {
            return Ok(RecordMutation::Keep(()));
        }
        record.report_message_id = Some(message_id);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

fn digest_fully_joined(paths: &StatePaths, message_id: &MessageId) -> Result<bool> {
    let mut found = false;
    for record in super::list(paths)?
        .into_iter()
        .filter(|record| record.report_message_id.as_ref() == Some(message_id))
    {
        found = true;
        if record.joined_at.is_none() {
            return Ok(false);
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
        let record = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "report".to_owned(),
            Path::new("/tmp/run-report").to_path_buf(),
        );
        super::super::create(&paths, &record).expect("create run");

        let (joined, _) = mark_joined(&paths, &record.run_id).expect("mark joined");
        let (joined_again, _) = mark_joined(&paths, &record.run_id).expect("repeat joined");
        assert_eq!(joined_again.joined_at, joined.joined_at);

        let first = MessageId::new();
        let second = MessageId::new();
        let reported = record_report_message(&paths, &record.run_id, first.clone())
            .expect("record report message");
        let reported_again =
            record_report_message(&paths, &record.run_id, second).expect("repeat report message");
        assert_eq!(reported.report_message_id.as_ref(), Some(&first));
        assert_eq!(reported_again.report_message_id.as_ref(), Some(&first));
    }

    #[test]
    fn digest_is_fully_joined_only_after_every_listed_run_is_joined() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/run-report-digest"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let message_id = MessageId::new();
        let records = ["first", "second"].map(|prompt| {
            let record = RunRecord::new(
                workspace_id.clone(),
                AgentKind::new_unchecked("codex"),
                PermissionMode::Auto,
                prompt.to_owned(),
                Path::new("/tmp/run-report-digest").to_path_buf(),
            );
            super::super::create(&paths, &record).expect("create run");
            record_report_message(&paths, &record.run_id, message_id.clone())
                .expect("record digest");
            record
        });

        assert!(!digest_fully_joined(&paths, &message_id).expect("unjoined digest"));
        mark_joined(&paths, &records[0].run_id).expect("join first");
        assert!(!digest_fully_joined(&paths, &message_id).expect("partially joined digest"));
        mark_joined(&paths, &records[1].run_id).expect("join second");
        assert!(digest_fully_joined(&paths, &message_id).expect("fully joined digest"));
        assert!(!digest_fully_joined(&paths, &MessageId::new()).expect("unknown digest"));
    }
}
