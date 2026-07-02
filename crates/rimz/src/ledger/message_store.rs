//! Live message queue store backed by one JSONL file per workspace.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::ids::MessageId;
use crate::ledger::atomic;
use crate::message::{MessageRecord, MessageStatus};

const QUEUE_FILE: &str = "messages.jsonl";
const LEGACY_TERMINAL_DIR: &str = "terminal";

#[derive(Debug, thiserror::Error)]
pub enum MessageStoreErr {
    #[error("message {0} not found")]
    NotFound(MessageId),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("io error on {path}: {source}")]
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
pub fn write(messages_dir: &Path, message: &MessageRecord) -> Result<()> {
    let mut messages = read_queue(messages_dir)?;
    match messages
        .iter_mut()
        .find(|existing| existing.message_id == message.message_id)
    {
        Some(existing) => *existing = message.clone(),
        None => messages.push(message.clone()),
    }
    write_queue(messages_dir, &messages)
}

#[must_use = "durability barrier; check the result"]
pub fn replace_all(messages_dir: &Path, messages: &[MessageRecord]) -> Result<()> {
    write_queue(messages_dir, messages)
}

#[must_use = "durability barrier; check the result"]
pub fn remove(messages_dir: &Path, message_id: &MessageId) -> Result<bool> {
    let mut messages = read_queue(messages_dir)?;
    let before = messages.len();
    messages.retain(|message| message.message_id != *message_id);
    let removed = messages.len() != before;
    if removed {
        write_queue(messages_dir, &messages)?;
    }
    Ok(removed)
}

pub fn load(messages_dir: &Path, message_id: &MessageId) -> Result<MessageRecord> {
    read_queue(messages_dir)?
        .into_iter()
        .find(|message| message.message_id == *message_id)
        .ok_or_else(|| MessageStoreErr::NotFound(message_id.clone()))
}

pub fn list(messages_dir: &Path) -> Result<Vec<MessageRecord>> {
    read_queue(messages_dir)
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
        if migrate_legacy(messages_dir)? {
            return read_queue_file(&path);
        }
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
    let last_non_empty = lines.iter().rposition(|line| !trim_ascii(line).is_empty());
    let mut messages = Vec::new();
    for (idx, line) in lines.into_iter().enumerate() {
        let line = trim_ascii(line);
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
    let path = queue_path(messages_dir);
    let mut messages = messages.to_vec();
    sort_messages(&mut messages);
    let mut bytes = Vec::new();
    for message in &messages {
        serde_json::to_writer(&mut bytes, message).map_err(|source| MessageStoreErr::Json {
            path: path.clone(),
            source,
        })?;
        bytes.push(b'\n');
    }
    atomic::write_bytes_atomically(&path, &bytes)?;
    Ok(())
}

fn migrate_legacy(messages_dir: &Path) -> Result<bool> {
    let terminal_dir = messages_dir.join(LEGACY_TERMINAL_DIR);
    let mut paths = legacy_json_paths(&terminal_dir)?;
    paths.extend(legacy_json_paths(messages_dir)?);
    if paths.is_empty() && !terminal_dir.exists() {
        return Ok(false);
    }

    let mut live = BTreeMap::new();
    for path in &paths {
        let message = read_legacy_message(path)?;
        if message.status.is_open() || message.status == MessageStatus::Sent {
            live.insert(message.message_id.to_string(), message);
        }
    }
    let messages = live.into_values().collect::<Vec<_>>();
    write_queue(messages_dir, &messages)?;

    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(MessageStoreErr::Io { path, source }),
        }
    }
    match fs::remove_dir_all(&terminal_dir) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(MessageStoreErr::Io {
                path: terminal_dir,
                source,
            });
        }
    }
    Ok(true)
}

fn legacy_json_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(MessageStoreErr::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| MessageStoreErr::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_legacy_message(path: &Path) -> Result<MessageRecord> {
    let bytes = fs::read(path).map_err(|source| MessageStoreErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| MessageStoreErr::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn queue_path(messages_dir: &Path) -> PathBuf {
    messages_dir.join(QUEUE_FILE)
}

fn sort_messages(messages: &mut [MessageRecord]) {
    messages.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::agents::{AgentState, AgentStatus};
    use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
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

        write(&messages_dir, &first).unwrap();
        write(&messages_dir, &second).unwrap();
        assert_eq!(list_pending(&messages_dir).unwrap().len(), 2);

        first.status = MessageStatus::Sent;
        write(&messages_dir, &first).unwrap();

        let messages = list(&messages_dir).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            load(&messages_dir, &first.message_id).unwrap().status,
            MessageStatus::Sent
        );
        assert_eq!(list_pending(&messages_dir).unwrap().len(), 1);
        assert!(queue_path(&messages_dir).exists());

        assert!(remove(&messages_dir, &first.message_id).unwrap());
        assert!(!remove(&messages_dir, &first.message_id).unwrap());
        let messages = list(&messages_dir).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, second.message_id);
    }

    #[test]
    fn legacy_layout_migrates_live_records_and_discards_terminal_records() {
        let dir = tempdir().unwrap();
        let messages_dir = dir.path().join("messages");
        let terminal_dir = messages_dir.join("terminal");
        std::fs::create_dir_all(&terminal_dir).unwrap();
        let agent = agent();
        let queued = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "queued".to_owned(),
            true,
            DeliveryGate::Done,
        );
        let mut sent = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "sent".to_owned(),
            true,
            DeliveryGate::Done,
        );
        sent.status = MessageStatus::Sent;
        let mut delivered = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "delivered".to_owned(),
            true,
            DeliveryGate::Done,
        );
        delivered.status = MessageStatus::Delivered;
        write_legacy(
            &messages_dir.join(format!("{}.json", queued.message_id)),
            &queued,
        );
        write_legacy(
            &terminal_dir.join(format!("{}.json", sent.message_id)),
            &sent,
        );
        write_legacy(
            &terminal_dir.join(format!("{}.json", delivered.message_id)),
            &delivered,
        );

        let messages = list(&messages_dir).unwrap();

        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .any(|message| message.message_id == queued.message_id)
        );
        assert!(
            messages
                .iter()
                .any(|message| message.message_id == sent.message_id)
        );
        assert!(queue_path(&messages_dir).exists());
        assert!(
            !messages_dir
                .join(format!("{}.json", queued.message_id))
                .exists()
        );
        assert!(!terminal_dir.exists());
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
        write(&messages_dir, &message).unwrap();
        let mut bytes = std::fs::read(queue_path(&messages_dir)).unwrap();
        bytes.extend_from_slice(b"{\"message_id\"");
        std::fs::write(queue_path(&messages_dir), bytes).unwrap();

        let messages = list(&messages_dir).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, message.message_id);
    }

    fn write_legacy(path: &Path, message: &MessageRecord) {
        std::fs::write(path, serde_json::to_vec_pretty(message).unwrap()).unwrap();
    }

    fn agent() -> AgentState {
        let now = jiff::Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from("sess-1"),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
