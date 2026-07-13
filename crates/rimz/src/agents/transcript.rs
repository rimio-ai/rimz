//! Adapter-normalized provider-native transcript message types.
//!
//! Adapters normalize their native transcript files into [`TranscriptMessage`].
//! The durable Rimz-owned cross-provider transcript log lives in [`crate::transcript`].

use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::AgentAdapter;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
}

/// Adapter-owned position in a provider-native transcript source.
///
/// JSONL adapters interpret this as a byte offset. Row-oriented stores can
/// interpret the same monotonic value as a row id while keeping callers
/// independent of the provider's storage format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TranscriptPosition(u64);

impl TranscriptPosition {
    pub const START: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One incremental page of normalized assistant output.
#[derive(Debug, PartialEq, Eq)]
pub struct TranscriptPage {
    pub next: TranscriptPosition,
    pub messages: Vec<String>,
}

/// Incremental reader for adapter-normalized assistant messages in one session.
///
/// The source identity includes the session id because row-backed adapters can
/// keep many conversations at one transcript path.
#[derive(Debug)]
pub struct TranscriptCursor {
    path: Option<String>,
    session_id: Option<String>,
    position: TranscriptPosition,
    skip_existing_on_first_path: bool,
}

impl TranscriptCursor {
    pub fn new(from_start: bool) -> Self {
        Self {
            path: None,
            session_id: None,
            position: TranscriptPosition::START,
            skip_existing_on_first_path: !from_start,
        }
    }

    pub fn messages(
        &mut self,
        transcript_path: Option<&str>,
        session_id: Option<&crate::ids::AgentSessionId>,
        adapter: &dyn AgentAdapter,
    ) -> Vec<String> {
        let Some(path) = transcript_path else {
            return Vec::new();
        };
        let path_ref = Path::new(path);
        let session_key = session_id.map(crate::ids::AgentSessionId::as_str);
        if self.path.as_deref() != Some(path) || self.session_id.as_deref() != session_key {
            self.position = if self.skip_existing_on_first_path {
                adapter
                    .transcript_position(path_ref, session_id)
                    .unwrap_or(TranscriptPosition::START)
            } else {
                TranscriptPosition::START
            };
            self.path = Some(path.to_owned());
            self.session_id = session_key.map(ToOwned::to_owned);
            self.skip_existing_on_first_path = false;
        }
        if adapter
            .transcript_position(path_ref, session_id)
            .is_some_and(|current| current < self.position)
        {
            self.position = TranscriptPosition::START;
        }
        let Some(page) =
            adapter.read_assistant_transcript_page(path_ref, session_id, self.position)
        else {
            return Vec::new();
        };
        self.position = page.next;
        page.messages
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn cursor_skips_existing_attach_bytes_and_resets_on_path_change() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.jsonl");
        std::fs::write(
            &first,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"old\"}}\n",
        )
        .unwrap();
        let first_path = first.to_string_lossy().into_owned();
        let mut cursor = TranscriptCursor::new(false);

        assert!(
            cursor
                .messages(Some(&first_path), None, &crate::agents::CodexAdapter)
                .is_empty(),
            "default attach starts at the current end"
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&first)
            .unwrap()
            .write_all(
                b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"new\"}}\n",
            )
            .unwrap();
        assert_eq!(
            cursor.messages(Some(&first_path), None, &crate::agents::CodexAdapter),
            vec!["new"]
        );

        let second = dir.path().join("second.jsonl");
        std::fs::write(
            &second,
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"fresh\"}}\n",
        )
        .unwrap();
        let second_path = second.to_string_lossy().into_owned();
        assert_eq!(
            cursor.messages(Some(&second_path), None, &crate::agents::CodexAdapter),
            vec!["fresh"],
            "a new transcript path starts at byte zero"
        );
    }
}
