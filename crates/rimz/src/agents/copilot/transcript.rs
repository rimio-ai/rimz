//! Normalization of Copilot's append-only per-session conversation events.

use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;

use super::super::{TranscriptMessage, TranscriptRole, sanitize_user_prompt};

#[derive(Deserialize)]
struct EventRecord {
    #[serde(rename = "type")]
    event_type: Option<String>,
    timestamp: Option<String>,
    data: Option<EventData>,
}

#[derive(Deserialize)]
struct EventData {
    content: Option<String>,
}

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| serde_json::from_str::<EventRecord>(line).ok())
        .filter_map(normalize_message)
        .collect()
}

pub(super) fn last_assistant_message(path: &Path) -> Option<String> {
    parse_messages(&std::fs::read_to_string(path).ok()?)
        .into_iter()
        .rev()
        .find(|message| message.role == TranscriptRole::Assistant)
        .map(|message| message.text)
}

fn normalize_message(record: EventRecord) -> Option<TranscriptMessage> {
    let role = match record.event_type.as_deref()? {
        "user.message" => TranscriptRole::User,
        "assistant.message" => TranscriptRole::Assistant,
        _ => return None,
    };
    let content = record.data?.content?;
    let text = match role {
        TranscriptRole::User => sanitize_user_prompt(Some(&content))?,
        TranscriptRole::Assistant => {
            if content.trim().is_empty() {
                return None;
            }
            content
        }
    };
    Some(TranscriptMessage {
        role,
        at: record
            .timestamp
            .as_deref()
            .and_then(|timestamp| timestamp.parse::<Timestamp>().ok()),
        text,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::agents::{AgentAdapter, CopilotAdapter, TranscriptPosition};

    const FIXTURE: &str = include_str!("tests/fixtures/events.jsonl");

    #[test]
    fn captured_events_normalize_only_visible_conversation_messages() {
        let messages = parse_messages(FIXTURE);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, TranscriptRole::User);
        assert_eq!(messages[0].text, "fixture prompt");
        assert_eq!(messages[1].role, TranscriptRole::Assistant);
        assert_eq!(messages[1].text, "fixture reply");
        assert_eq!(messages[2].text, "second reply");
        assert_eq!(
            messages[1].at,
            Some("2026-07-13T15:13:23.939Z".parse().unwrap())
        );
        assert_eq!(
            crate::agents::turns::session_turns(&messages, &[], "session-fixture", false).len(),
            1
        );
    }

    #[test]
    fn empty_control_malformed_and_unknown_records_are_ignored() {
        let lines = r#"
{"type":"user.message","timestamp":"bad","data":{"content":"<system-reminder>noise"}}
{"type":"assistant.message","data":{"content":"  "}}
{"type":"tool.message","data":{"content":"tool output"}}
{}
not-json
"#;
        assert!(parse_messages(lines).is_empty());
    }

    #[test]
    fn incremental_page_retains_a_torn_line_then_emits_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"assistant.message\",\"data\":{\"content\":\"one\"}}\n{\"type\":\"assistant.message\",\"data\":{\"content\":",
        )
        .unwrap();
        let first = CopilotAdapter
            .read_assistant_transcript_page(&path, None, TranscriptPosition::START)
            .unwrap();
        assert_eq!(first.messages, vec!["one"]);

        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\"two\"}}")
            .unwrap();
        let second = CopilotAdapter
            .read_assistant_transcript_page(&path, None, first.next)
            .unwrap();
        assert_eq!(second.messages, vec!["two"]);
        assert!(
            CopilotAdapter
                .read_assistant_transcript_page(&path, None, second.next)
                .is_none()
        );
    }

    #[test]
    fn final_assistant_selection_ignores_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, FIXTURE).unwrap();
        assert_eq!(
            last_assistant_message(&path).as_deref(),
            Some("second reply")
        );
    }
}
