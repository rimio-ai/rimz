//! Live message queue store backed by one JSONL file per workspace.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use super::{MessageRecord, MessageStatus};
use crate::disk::atomic;
use crate::disk::buckets::{bucket_file_name, bucket_files};
use crate::disk::retention::TRANSCRIPT_FILE_DAYS;

const QUEUE_FILE: &str = "messages.jsonl";
const HISTORY_READ_LIMIT: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum MessageStoreErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub(super) type Result<T> = std::result::Result<T, MessageStoreErr>;

pub(in crate::store) fn append_history_many(
    messages_dir: &Path,
    messages: &[MessageRecord],
) -> Result<()> {
    let mut buckets = std::collections::BTreeMap::<PathBuf, Vec<u8>>::new();
    for message in messages {
        let path = messages_dir.join(bucket_file_name(message.updated_at, TRANSCRIPT_FILE_DAYS));
        let bytes = buckets.entry(path.clone()).or_default();
        serde_json::to_writer(&mut *bytes, message).map_err(|source| MessageStoreErr::Json {
            path: path.clone(),
            source,
        })?;
        bytes.push(b'\n');
    }
    for (path, bytes) in buckets {
        atomic::append_record_bytes(&path, &bytes)?;
    }
    Ok(())
}

pub(in crate::store) fn list_history(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    let mut files = bucket_files(messages_dir).map_err(|source| MessageStoreErr::Io {
        path: messages_dir.to_path_buf(),
        source,
    })?;
    files.sort();
    let mut messages = Vec::new();
    for path in files.into_iter().rev() {
        messages.extend(
            read_queue_file(&path)?
                .into_iter()
                .rev()
                .take(HISTORY_READ_LIMIT - messages.len()),
        );
        if messages.len() == HISTORY_READ_LIMIT {
            break;
        }
    }
    sort_messages(&mut messages);
    Ok(messages)
}

pub(in crate::store) fn list_pending(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    Ok(read_queue(messages_dir)?
        .into_iter()
        .filter(|message| message.status == MessageStatus::Queued)
        .collect())
}

pub(in crate::store) fn read_queue(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    let path = queue_path(messages_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_queue_file(&path)
}

fn read_queue_file(path: &Path) -> Result<Vec<MessageRecord>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(MessageStoreErr::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let tail_terminated = bytes.ends_with(b"\n");
    let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    let last_non_empty = lines.iter().rposition(|line| !line.trim_ascii().is_empty());
    let mut messages = Vec::new();
    for (idx, line) in lines.into_iter().enumerate() {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<MessageRecord>(line) {
            Ok(message) => messages.push(message),
            Err(source) if Some(idx) == last_non_empty && !tail_terminated => {
                warn!(
                    path = %path.display(),
                    line = idx + 1,
                    error = %source,
                    "skipping torn trailing message queue record"
                );
                break;
            }
            Err(source) => {
                return Err(MessageStoreErr::Json {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    sort_messages(&mut messages);
    Ok(messages)
}

#[must_use = "durability barrier; check the result"]
pub(in crate::store) fn write_queue(messages_dir: &Path, messages: &[MessageRecord]) -> Result<()> {
    write_messages_file(&queue_path(messages_dir), messages)
}

fn write_messages_file(path: &Path, messages: &[MessageRecord]) -> Result<()> {
    let mut messages = messages.to_vec();
    sort_messages(&mut messages);
    let mut bytes = Vec::new();
    for message in &messages {
        serde_json::to_writer(&mut bytes, message).map_err(|source| MessageStoreErr::Json {
            path: path.to_path_buf(),
            source,
        })?;
        bytes.push(b'\n');
    }
    atomic::write_bytes_atomically(path, &bytes)?;
    Ok(())
}

fn queue_path(messages_dir: &Path) -> PathBuf {
    messages_dir.join(QUEUE_FILE)
}

fn sort_messages(messages: &mut [MessageRecord]) {
    messages.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::agents::AgentState;
    use crate::ids::{MessageId, WorkspaceId};
    use crate::store::message::DeliveryGate;

    #[test]
    fn missing_messages_dir_lists_empty() {
        let dir = tempdir().unwrap();
        assert!(
            list_pending(&dir.path().join("messages"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn single_file_queue_round_trips_and_removes_records() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let agent = agent();
        let mut first = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "first".to_owned(),
            DeliveryGate::Done,
        );
        let second = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "second".to_owned(),
            DeliveryGate::Done,
        );

        write_queue(&messages_dir, &[first.clone(), second.clone()]).unwrap();
        assert_eq!(list_pending(&messages_dir).unwrap().len(), 2);

        first.status = MessageStatus::Sent;
        write_queue(&messages_dir, &[first.clone(), second.clone()]).unwrap();

        let mut messages = read_queue(&messages_dir).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .iter()
                .find(|message| message.message_id == first.message_id)
                .unwrap()
                .status,
            MessageStatus::Sent
        );
        assert_eq!(list_pending(&messages_dir).unwrap().len(), 1);
        assert!(queue_path(&messages_dir).exists());

        messages.retain(|message| message.message_id != first.message_id);
        write_queue(&messages_dir, &messages).unwrap();
        let messages = read_queue(&messages_dir).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, second.message_id);
    }

    #[test]
    fn history_round_trips_terminal_text() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let agent = agent();
        let mut message = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "delivered body".to_owned(),
            DeliveryGate::Done,
        );
        message.status = MessageStatus::Delivered;

        append_history_many(&messages_dir, std::slice::from_ref(&message)).unwrap();

        let history = list_history(&messages_dir).unwrap();
        assert_eq!(history, vec![message]);
    }

    #[test]
    fn history_batch_preserves_append_order() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let agent = agent();
        let mut first = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "first".to_owned(),
            DeliveryGate::Done,
        );
        first.message_id = fixed_message_id(2);
        first.status = MessageStatus::Delivered;
        let mut second = first.clone();
        second.message_id = fixed_message_id(1);
        second.text = "second".to_owned();

        append_history_many(&messages_dir, &[first.clone(), second.clone()]).unwrap();

        let path = messages_dir.join(bucket_file_name(first.updated_at, TRANSCRIPT_FILE_DAYS));
        assert_eq!(list_history(&messages_dir).unwrap(), vec![second, first]);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(raw.find("first").unwrap() < raw.find("second").unwrap());
    }

    #[test]
    fn history_reads_newest_records_without_opening_older_buckets() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let agent = agent();
        for index in 0..=500 {
            let mut message = MessageRecord::new(
                WorkspaceId::from_project_root(dir.path()),
                &agent,
                "x".repeat(2048),
                DeliveryGate::Done,
            );
            message.message_id = fixed_message_id(index as u64);
            message.status = MessageStatus::Delivered;
            message.updated_at = jiff::Timestamp::from_second(if index == 0 {
                0
            } else if index < 251 {
                604800
            } else {
                1209600
            })
            .unwrap();
            append_history_many(&messages_dir, std::slice::from_ref(&message)).unwrap();
        }

        // An unreadable older bucket proves the reader stops at its bound.
        fs::write(messages_dir.join("1970-01-01.jsonl"), b"invalid json\n").unwrap();
        let history = list_history(&messages_dir).unwrap();
        assert_eq!(history.len(), HISTORY_READ_LIMIT);
        assert_eq!(history[0].message_id, fixed_message_id(1));
        assert_eq!(history[499].message_id, fixed_message_id(500));
        assert!(messages_dir.join("1970-01-01.jsonl").is_file());
        assert!(messages_dir.join("1970-01-08.jsonl").is_file());
    }

    #[test]
    fn unknown_harness_notice_round_trips() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let history_dir = dir.path().join("history");
        let message = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent(),
            "x".repeat(2048),
            DeliveryGate::Done,
        );
        let mut value = serde_json::to_value(message).unwrap();
        value["sender"] = serde_json::json!({"origin": "harness", "notice": "future_notice"});
        let message: MessageRecord = serde_json::from_value(value.clone()).unwrap();
        write_queue(&messages_dir, std::slice::from_ref(&message)).unwrap();
        let queued = read_queue(&messages_dir).unwrap();
        write_queue(&messages_dir, &queued).unwrap();
        assert_eq!(
            serde_json::to_value(&read_queue(&messages_dir).unwrap()[0]).unwrap(),
            value
        );

        for index in 0..=500 {
            let mut terminal = message.clone();
            terminal.message_id = fixed_message_id(index as u64);
            terminal.status = MessageStatus::Delivered;
            append_history_many(&history_dir, &[terminal]).unwrap();
        }
        let history = list_history(&history_dir).unwrap();
        assert_eq!(history.len(), HISTORY_READ_LIMIT);
        for record in history {
            assert_eq!(
                serde_json::to_value(record).unwrap()["sender"]["notice"],
                "future_notice"
            );
        }
        value["sender"]["notice"] = serde_json::json!(42);
        assert!(serde_json::from_value::<MessageRecord>(value).is_err());
    }

    #[test]
    fn torn_trailing_line_is_ignored() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let agent = agent();
        let message = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "queued".to_owned(),
            DeliveryGate::Done,
        );
        write_queue(&messages_dir, std::slice::from_ref(&message)).unwrap();
        let mut bytes = std::fs::read(queue_path(&messages_dir)).unwrap();
        bytes.extend_from_slice(b"{\"message_id\"");
        std::fs::write(queue_path(&messages_dir), bytes).unwrap();

        let messages = read_queue(&messages_dir).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, message.message_id);
    }

    fn fixed_message_id(value: u64) -> MessageId {
        MessageId::parse(&format!("msg_{value:016}")).unwrap()
    }

    fn agent() -> AgentState {
        let now = jiff::Timestamp::now();
        crate::testkit::agent_state("claude", "sess-1", now)
    }
}
