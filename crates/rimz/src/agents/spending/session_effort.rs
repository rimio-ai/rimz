//! Durable, home-wide active-time releases and conversation counts.

use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::{self, AtomicErr};
use crate::disk::paths::RuntimePaths;
use crate::ids::{AgentKind, AgentSessionId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEffortRecord {
    pub kind: AgentKind,
    pub session: AgentSessionId,
    pub at: Timestamp,
    #[serde(flatten)]
    pub event: SessionEffortEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEffortEvent {
    Active {
        active_ms: u64,
    },
    FromUser,
    FromAgent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_kind: Option<AgentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_session: Option<AgentSessionId>,
    },
    Totals {
        active_ms: u64,
        from_user: u64,
        from_teammates: u64,
        to_teammates: u64,
    },
}

#[must_use = "durability barrier; check the result"]
pub fn record(paths: &RuntimePaths, record: &SessionEffortRecord) -> Result<(), AtomicErr> {
    let date = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
    let path = paths.shared_session_effort(date);
    if let Some(directory) = path.parent()
        && !directory.exists()
    {
        std::fs::create_dir_all(directory).map_err(|source| AtomicErr::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        if let Some(providers) = directory.parent() {
            atomic::sync_dir(providers)?;
        }
    }
    // Separate a torn prior tail without reading the segment on the hook path.
    let mut bytes = vec![b'\n'];
    serde_json::to_writer(&mut bytes, record)?;
    bytes.push(b'\n');
    atomic::append_record_bytes(&path, &bytes)?;
    atomic::sync_file_data(&path)?;
    Ok(())
}

/// Decode intact records, ignoring unknown fields and malformed lines.
/// Missing or unreadable segments contribute no records.
pub fn read_segment(path: &Path) -> Vec<SessionEffortRecord> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn append_after_a_torn_tail_preserves_complete_records_and_decodes_totals() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        let date = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date();
        let path = paths.shared_session_effort(date);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{\"kind\":\"claude\",\"session\":\"old\",\"at\":\"2026-01-01T00:00:00Z\",\"event\":\"totals\",\"active_ms\":20,\"from_user\":1,\"from_teammates\":2,\"to_teammates\":3,\"future\":true}\nnot json\n{\"kind\":").unwrap();
        let appended = SessionEffortRecord {
            kind: AgentKind::new_unchecked("codex"),
            session: "new".into(),
            at: "2025-12-31T23:59:59Z".parse().unwrap(),
            event: SessionEffortEvent::Active { active_ms: 30 },
        };
        record(&paths, &appended).unwrap();
        let records = read_segment(&path);
        assert_eq!(
            records.len(),
            2,
            "only the malformed/torn records may be lost"
        );
        assert!(matches!(
            records[0].event,
            SessionEffortEvent::Totals {
                active_ms: 20,
                from_user: 1,
                from_teammates: 2,
                to_teammates: 3
            }
        ));
        assert_eq!(
            records[1], appended,
            "a replayed event lands in today's segment"
        );
    }
}
