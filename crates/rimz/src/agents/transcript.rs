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

/// Incremental reader for adapter-normalized assistant messages in one transcript.
#[derive(Debug)]
pub struct TranscriptCursor {
    path: Option<String>,
    offset: u64,
    skip_existing_on_first_path: bool,
}

impl TranscriptCursor {
    pub fn new(from_start: bool) -> Self {
        Self {
            path: None,
            offset: 0,
            skip_existing_on_first_path: !from_start,
        }
    }

    pub fn messages(
        &mut self,
        transcript_path: Option<&str>,
        adapter: &dyn AgentAdapter,
    ) -> Vec<String> {
        let Some(path) = transcript_path else {
            return Vec::new();
        };
        if self.path.as_deref() != Some(path) {
            self.offset = if self.skip_existing_on_first_path {
                std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
            } else {
                0
            };
            self.path = Some(path.to_owned());
            self.skip_existing_on_first_path = false;
        }
        if std::fs::metadata(path)
            .map(|meta| meta.len() < self.offset)
            .unwrap_or(false)
        {
            self.offset = 0;
        }
        let Some((bytes, next)) = super::read_transcript_lines(Path::new(path), self.offset) else {
            return Vec::new();
        };
        self.offset = next;
        let text = String::from_utf8_lossy(&bytes);
        adapter.stream_assistant_messages(&text)
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
                .messages(Some(&first_path), &crate::agents::CodexAdapter)
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
            cursor.messages(Some(&first_path), &crate::agents::CodexAdapter),
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
            cursor.messages(Some(&second_path), &crate::agents::CodexAdapter),
            vec!["fresh"],
            "a new transcript path starts at byte zero"
        );
    }
}
