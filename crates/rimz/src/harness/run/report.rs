//! Exactly-once handshake between inline joins and subagent reports.

use crate::ids::{MessageId, RunId};
use crate::store::StatePaths;

use super::{RecordMutation, Result, RunRecord, update_record};

pub fn mark_joined(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    update_record(paths, run_id, |record, now| {
        if record.joined_at.is_some() {
            return Ok(RecordMutation::Keep(()));
        }
        record.joined_at = Some(now);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
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

        let joined = mark_joined(&paths, &record.run_id).expect("mark joined");
        let joined_again = mark_joined(&paths, &record.run_id).expect("repeat joined");
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
}
