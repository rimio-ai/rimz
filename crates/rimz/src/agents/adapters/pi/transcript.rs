//! Pi provider-native conversation normalization.
//!
//! Pi persists one JSONL tree entry per message. Conversation surfaces keep
//! visible user and assistant text in append order; tool results, thinking,
//! tool calls, summaries, and extension records stay out of the stream.

use serde::Deserialize;
use serde_json::Value;

use super::super::{TranscriptMessage, TranscriptRole, sanitize_user_prompt};

#[derive(Deserialize)]
struct PiTranscriptEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    timestamp: Option<String>,
    message: Option<PiConversationMessage>,
}

#[derive(Deserialize)]
struct PiConversationMessage {
    role: Option<String>,
    content: Option<Value>,
}

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| {
            let entry = serde_json::from_str::<PiTranscriptEntry>(line.trim()).ok()?;
            if entry.entry_type.as_deref() != Some("message") {
                return None;
            }
            let message = entry.message?;
            let role = match message.role.as_deref() {
                Some("user") => TranscriptRole::User,
                Some("assistant") => TranscriptRole::Assistant,
                _ => return None,
            };
            let visible = visible_text(message.content.as_ref()?)?;
            let text = match role {
                TranscriptRole::User => sanitize_user_prompt(Some(&visible))?,
                TranscriptRole::Assistant => visible,
            };
            Some(TranscriptMessage {
                role,
                at: entry.timestamp.as_deref().and_then(|raw| raw.parse().ok()),
                text,
            })
        })
        .collect()
}

fn visible_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(text) => text.trim().to_owned(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::super::PiAdapter;
    use super::*;
    use crate::agents::capabilities::{SpendingCapability as _, TranscriptCapability as _};

    #[test]
    fn normalizes_visible_user_and_assistant_text() {
        let lines = concat!(
            r#"{"type":"session","timestamp":"2026-06-02T09:00:00.000Z"}"#,
            "\n",
            r#"{"type":"message","id":"u1","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"user","content":"  fix the parser  "}}"#,
            "\n",
            r#"{"type":"message","id":"a1","parentId":"u1","timestamp":"2026-06-02T10:00:01.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"First line"},{"type":"toolCall","name":"read"},{"type":"text","text":"Second line"}]}}"#,
            "\n",
            r#"{"type":"message","id":"t1","message":{"role":"toolResult","content":[{"type":"text","text":"hidden"}]}}"#,
            "\n",
            r#"{"type":"message","id":"u2","message":{"role":"user","content":"<system-reminder>control"}}"#,
        );

        let messages = parse_messages(lines);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, TranscriptRole::User);
        assert_eq!(messages[0].text, "fix the parser");
        assert_eq!(messages[1].role, TranscriptRole::Assistant);
        assert_eq!(messages[1].text, "First line\nSecond line");
        assert!(messages.iter().all(|message| message.at.is_some()));
    }

    #[test]
    fn adapter_reads_a_jsonl_source_and_streams_assistant_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let line = r#"{"type":"message","timestamp":"2026-06-02T10:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#;
        std::fs::write(&path, format!("{line}\n")).unwrap();

        let messages = PiAdapter.read_transcript_messages(&path, None).unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "done");
        assert_eq!(PiAdapter.stream_assistant_messages(line), vec!["done"]);
    }

    #[test]
    fn normalized_conversation_and_spend_produce_one_history_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"message","timestamp":"2026-06-02T10:00:00.000Z","message":{"role":"user","content":"fix history"}}"#,
                "\n",
                r#"{"type":"message","timestamp":"2026-06-02T10:00:01.000Z","message":{"role":"assistant","model":"gpt-5","content":[{"type":"text","text":"done"}],"usage":{"input":100,"output":50,"cost":{"total":0.42}}}}"#,
                "\n",
            ),
        )
        .unwrap();
        let adapter = PiAdapter;
        let prices = crate::agents::PriceBook::fixture();
        let messages = adapter.read_transcript_messages(&path, None).unwrap();
        let spend = adapter.parse_spend(&path, None, &prices);

        let turns =
            crate::agents::turns::session_turns(&messages, &spend.entries, "session", false);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt, "fix history");
        assert_eq!(turns[0].fresh_input, 100);
        assert_eq!(turns[0].output, 50);
        assert_eq!(turns[0].cost_usd, Some(0.42));
        assert_eq!(turns[0].outcome, crate::agents::turns::TurnOutcome::Done);
    }
}
