//! Live message queue store backed by one JSONL file per workspace.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::message::{MessageRecord, MessageStatus};
use crate::store::atomic;

const QUEUE_FILE: &str = "messages.jsonl";
const HISTORY_FILE: &str = "history.jsonl";
const HISTORY_MAX_BYTES: u64 = 512 * 1024;
const HISTORY_KEEP_RECORDS: usize = 500;

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

pub type Result<T> = std::result::Result<T, MessageStoreErr>;

#[must_use = "durability barrier; check the result"]
pub fn replace_all(messages_dir: &Path, messages: &[MessageRecord]) -> Result<()> {
    write_queue(messages_dir, messages)
}

pub fn list(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    read_queue(messages_dir)
}

#[must_use = "durability barrier; check the result"]
pub fn append_history_many(messages_dir: &Path, messages: &[MessageRecord]) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let path = history_path(messages_dir);
    let mut bytes = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut bytes, message).map_err(|source| MessageStoreErr::Json {
            path: path.clone(),
            source,
        })?;
        bytes.push(b'\n');
    }
    atomic::append_record_bytes(&path, &bytes)?;
    let size = fs::metadata(&path)
        .map_err(|source| MessageStoreErr::Io {
            path: path.clone(),
            source,
        })?
        .len();
    if size > HISTORY_MAX_BYTES {
        prune_history(&path)?;
    }
    Ok(())
}

pub fn list_history(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    read_queue_file(&history_path(messages_dir))
}

pub fn list_pending(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    Ok(read_queue(messages_dir)?
        .into_iter()
        .filter(|message| message.status == MessageStatus::Queued)
        .collect())
}

fn read_queue(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
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

fn write_queue(messages_dir: &Path, messages: &[MessageRecord]) -> Result<()> {
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

fn prune_history(path: &Path) -> Result<()> {
    let mut messages = read_queue_file(path)?;
    if messages.len() <= HISTORY_KEEP_RECORDS {
        return Ok(());
    }
    sort_messages(&mut messages);
    let keep_from = messages.len() - HISTORY_KEEP_RECORDS;
    write_messages_file(path, &messages[keep_from..])?;
    Ok(())
}

fn queue_path(messages_dir: &Path) -> PathBuf {
    messages_dir.join(QUEUE_FILE)
}

fn history_path(messages_dir: &Path) -> PathBuf {
    messages_dir.join(HISTORY_FILE)
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
    use crate::message::DeliveryGate;

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
            true,
            DeliveryGate::Done,
        );
        let second = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "second".to_owned(),
            true,
            DeliveryGate::Done,
        );

        replace_all(&messages_dir, &[first.clone(), second.clone()]).unwrap();
        assert_eq!(list_pending(&messages_dir).unwrap().len(), 2);

        first.status = MessageStatus::Sent;
        replace_all(&messages_dir, &[first.clone(), second.clone()]).unwrap();

        let mut messages = list(&messages_dir).unwrap();
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
        replace_all(&messages_dir, &messages).unwrap();
        let messages = list(&messages_dir).unwrap();
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
            true,
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
            true,
            DeliveryGate::Done,
        );
        first.message_id = fixed_message_id(2);
        first.status = MessageStatus::Delivered;
        let mut second = first.clone();
        second.message_id = fixed_message_id(1);
        second.text = "second".to_owned();

        append_history_many(&messages_dir, &[first.clone(), second.clone()]).unwrap();

        assert_eq!(list_history(&messages_dir).unwrap(), vec![second, first]);
        let raw = std::fs::read_to_string(history_path(&messages_dir)).unwrap();
        assert!(raw.find("first").unwrap() < raw.find("second").unwrap());
    }

    #[test]
    fn history_prunes_to_newest_records() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let agent = agent();
        for index in 0..=HISTORY_KEEP_RECORDS {
            let mut message = MessageRecord::new(
                WorkspaceId::from_project_root(dir.path()),
                &agent,
                "x".repeat(2048),
                true,
                DeliveryGate::Done,
            );
            message.message_id = fixed_message_id(index as u64);
            message.status = MessageStatus::Delivered;
            append_history_many(&messages_dir, std::slice::from_ref(&message)).unwrap();
        }

        let history = list_history(&messages_dir).unwrap();

        assert_eq!(history.len(), HISTORY_KEEP_RECORDS);
        assert_eq!(history[0].message_id, fixed_message_id(1));
        assert_eq!(
            history[HISTORY_KEEP_RECORDS - 1].message_id,
            fixed_message_id(HISTORY_KEEP_RECORDS as u64)
        );
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
            true,
            DeliveryGate::Done,
        );
        replace_all(&messages_dir, std::slice::from_ref(&message)).unwrap();
        let mut bytes = std::fs::read(queue_path(&messages_dir)).unwrap();
        bytes.extend_from_slice(b"{\"message_id\"");
        std::fs::write(queue_path(&messages_dir), bytes).unwrap();

        let messages = list(&messages_dir).unwrap();

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
