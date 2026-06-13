//! Per-agent queued-message files: `queue/<message_id>.json` while pending,
//! `queue/terminal/<message_id>.json` once delivered, removed, or abandoned.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ids::MessageId;
use crate::ledger::atomic::{self, write_temp_then_rename_cache};
use crate::message::{MessageRecord, MessageStatus};

#[derive(Debug, thiserror::Error)]
pub enum MessageStoreErr {
    #[error("message {0} not found")]
    NotFound(MessageId),
    #[error("message {message_id} is not pending (status = {status})")]
    NotPending {
        message_id: MessageId,
        status: MessageStatus,
    },
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

const TERMINAL_SUBDIR: &str = "terminal";

fn pending_path(queue_dir: &Path, message_id: &MessageId) -> PathBuf {
    queue_dir.join(format!("{message_id}.json"))
}

fn terminal_path(queue_dir: &Path, message_id: &MessageId) -> PathBuf {
    queue_dir
        .join(TERMINAL_SUBDIR)
        .join(format!("{message_id}.json"))
}

#[must_use = "durability barrier; check the result"]
pub fn write(queue_dir: &Path, message: &MessageRecord) -> Result<()> {
    let path = pending_path(queue_dir, &message.message_id);
    write_temp_then_rename_cache(&path, message)?;
    if message.status.leaves_pending_queue() {
        let dest = terminal_path(queue_dir, &message.message_id);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| MessageStoreErr::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::rename(&path, &dest).map_err(|source| MessageStoreErr::Io { path, source })?;
    } else {
        remove_terminal_copy(queue_dir, &message.message_id)?;
    }
    Ok(())
}

pub fn load(queue_dir: &Path, message_id: &MessageId) -> Result<MessageRecord> {
    let pending = pending_path(queue_dir, message_id);
    let path = if pending.exists() {
        pending
    } else {
        let terminal = terminal_path(queue_dir, message_id);
        if !terminal.exists() {
            return Err(MessageStoreErr::NotFound(message_id.clone()));
        }
        terminal
    };
    read_item(&path)
}

pub fn list(queue_dir: &Path) -> Result<Vec<MessageRecord>> {
    let mut by_id = std::collections::HashMap::new();
    for item in read_dir_items(&queue_dir.join(TERMINAL_SUBDIR))?
        .into_iter()
        .chain(read_dir_items(queue_dir)?)
    {
        by_id.insert(item.message_id.clone(), item);
    }
    let mut items: Vec<MessageRecord> = by_id.into_values().collect();
    items.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
    Ok(items)
}

pub fn list_pending(queue_dir: &Path) -> Result<Vec<MessageRecord>> {
    let mut items: Vec<MessageRecord> = read_dir_items(queue_dir)?
        .into_iter()
        .filter(|item| item.status == MessageStatus::Pending)
        .collect();
    items.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
    Ok(items)
}

fn read_dir_items(dir: &Path) -> Result<Vec<MessageRecord>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| MessageStoreErr::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| MessageStoreErr::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        items.push(read_item(&path)?);
    }
    Ok(items)
}

fn read_item(path: &Path) -> Result<MessageRecord> {
    let bytes = fs::read(path).map_err(|source| MessageStoreErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| MessageStoreErr::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn remove_terminal_copy(queue_dir: &Path, message_id: &MessageId) -> Result<()> {
    let path = terminal_path(queue_dir, message_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MessageStoreErr::Io { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::feed::{AgentState, AgentStatus};
    use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
    use crate::message::DeliveryGate;

    #[test]
    fn missing_queue_dir_lists_empty() {
        let dir = tempdir().unwrap();
        assert!(list_pending(&dir.path().join("queue")).unwrap().is_empty());
    }

    #[test]
    fn terminal_status_relocates_out_of_pending_scan() {
        let dir = tempdir().unwrap();
        let agent = agent();
        let mut message = MessageRecord::new(
            WorkspaceId::from_project_root(dir.path()),
            &agent,
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        write(dir.path(), &message).unwrap();
        assert_eq!(list_pending(dir.path()).unwrap().len(), 1);

        message.status = MessageStatus::Claimed;
        write(dir.path(), &message).unwrap();

        assert!(list_pending(dir.path()).unwrap().is_empty());
        assert_eq!(list(dir.path()).unwrap().len(), 1);
        assert_eq!(
            load(dir.path(), &message.message_id).unwrap().status,
            MessageStatus::Claimed
        );

        message.status = MessageStatus::Pending;
        write(dir.path(), &message).unwrap();

        assert_eq!(list_pending(dir.path()).unwrap().len(), 1);
        assert_eq!(list(dir.path()).unwrap().len(), 1);
        assert_eq!(
            load(dir.path(), &message.message_id).unwrap().status,
            MessageStatus::Pending
        );
    }

    fn agent() -> AgentState {
        let now = jiff::Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from("sess-1"),
            kind: AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
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
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
