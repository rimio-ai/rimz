//! Durable notification trace log.
//!
//! A tab-bell notification with no matching unread card is invisible after the
//! fact, so the producer's emitted notifications, each renderer's bell decision,
//! and the unread mark/clear transitions append compact JSONL records under the
//! workspace state directory. The log is diagnostic state: append-only within a
//! size cap, never read by correctness code. Records are written through
//! [`super::DiagSink`], which already carries the workspace identity to
//! every emission site.

use std::path::Path;

use super::JsonlLog;

const NOTIFY_LOG_NAME: &str = "notify.log.jsonl";
const NOTIFY_LOG_MAX_BYTES: u64 = 1_048_576;

pub fn log(state_root: &Path) -> JsonlLog {
    JsonlLog::new(state_root.join(NOTIFY_LOG_NAME), NOTIFY_LOG_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_writes_jsonl_record() {
        let dir = tempfile::tempdir().unwrap();
        let log = log(dir.path());
        log.append(&serde_json::json!({ "event": "bell_ring" }));

        let bytes = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(bytes, "{\"event\":\"bell_ring\"}\n");
    }
}
