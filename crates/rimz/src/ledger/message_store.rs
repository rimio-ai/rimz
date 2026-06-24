//! Per-agent queued-message files: `queue/<message_id>.json` while queued,
//! `queue/terminal/<message_id>.json` once claimed, sent, or terminal.

use std::io;
use std::path::{Path, PathBuf};

use crate::ids::MessageId;
use crate::ledger::atomic;
use crate::ledger::pending_terminal::{self, PendingTerminalRecord};
use crate::message::{MessageRecord, MessageStatus};

#[derive(Debug, thiserror::Error)]
pub enum MessageStoreErr {
    #[error("message {0} not found")]
    NotFound(MessageId),
    #[error("message {message_id} is not queued (status = {status})")]
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

impl From<pending_terminal::StoreErr> for MessageStoreErr {
    fn from(err: pending_terminal::StoreErr) -> Self {
        match err {
            pending_terminal::StoreErr::Atomic(err) => Self::Atomic(err),
            pending_terminal::StoreErr::Io { path, source } => Self::Io { path, source },
            pending_terminal::StoreErr::Json { path, source } => Self::Json { path, source },
        }
    }
}

impl PendingTerminalRecord for MessageRecord {
    fn file_stem(&self) -> String {
        self.message_id.to_string()
    }

    fn is_terminal(&self) -> bool {
        self.status.leaves_pending_queue()
    }
}

#[must_use = "durability barrier; check the result"]
pub fn write(queue_dir: &Path, message: &MessageRecord) -> Result<()> {
    pending_terminal::write(queue_dir, message)?;
    Ok(())
}

pub fn load(queue_dir: &Path, message_id: &MessageId) -> Result<MessageRecord> {
    let stem = message_id.to_string();
    pending_terminal::load::<MessageRecord>(queue_dir, &stem)?
        .ok_or_else(|| MessageStoreErr::NotFound(message_id.clone()))
}

pub fn list(queue_dir: &Path) -> Result<Vec<MessageRecord>> {
    let mut items = pending_terminal::list_all::<MessageRecord>(queue_dir)?;
    items.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
    Ok(items)
}

pub fn list_pending(queue_dir: &Path) -> Result<Vec<MessageRecord>> {
    let mut items: Vec<MessageRecord> =
        pending_terminal::list_pending_raw::<MessageRecord>(queue_dir)?
            .into_iter()
            .filter(|item| item.status == MessageStatus::Queued)
            .collect();
    items.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
    Ok(items)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::agents::{AgentState, AgentStatus};
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

        message.status = MessageStatus::Queued;
        write(dir.path(), &message).unwrap();

        assert_eq!(list_pending(dir.path()).unwrap().len(), 1);
        assert_eq!(list(dir.path()).unwrap().len(), 1);
        assert_eq!(
            load(dir.path(), &message.message_id).unwrap().status,
            MessageStatus::Queued
        );
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
