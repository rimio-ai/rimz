//! Gemini conversation projection and rewind-aware incremental assistant output.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Take};
use std::path::Path;

use jiff::Timestamp;
use serde_json::Value;

use super::payloads::{FoldedTranscript, GeminiMessage, TranscriptChange, fold_transcript};
use crate::agents::{
    TranscriptMessage, TranscriptPage, TranscriptPosition, TranscriptRole, read_transcript_lines,
    sanitize_user_prompt,
};

pub(super) fn messages(text: &str) -> Vec<TranscriptMessage> {
    project(&fold_transcript(text))
}

pub(super) fn assistant_page(path: &Path, position: TranscriptPosition) -> Option<TranscriptPage> {
    let (suffix, next) = read_transcript_lines(path, position.get())?;
    let suffix = String::from_utf8_lossy(&suffix);
    if position == TranscriptPosition::START {
        return Some(TranscriptPage {
            next: TranscriptPosition::new(next),
            messages: fold_transcript(&suffix)
                .messages
                .iter()
                .filter_map(assistant_text)
                .collect(),
        });
    }

    let changes = FoldedTranscript::default().apply(&suffix);
    if !changes.iter().any(|change| {
        matches!(
            change,
            TranscriptChange::Control
                | TranscriptChange::Ordinary {
                    assistant: true,
                    ..
                }
        )
    }) {
        return Some(TranscriptPage {
            next: TranscriptPosition::new(next),
            messages: Vec::new(),
        });
    }

    let prefix = read_prefix(path, position.get())?;
    let mut folded = fold_transcript(&String::from_utf8_lossy(&prefix));
    let changes = folded.apply(&suffix);
    let mut seen = BTreeSet::new();
    let introduced = changes.into_iter().filter_map(|change| match change {
        TranscriptChange::Ordinary {
            id: Some(id),
            assistant: true,
            introduced: true,
        } if seen.insert(id.clone()) => Some(id),
        _ => None,
    });
    let messages = introduced
        .filter_map(|id| {
            folded
                .messages
                .iter()
                .find(|message| message.id.as_deref() == Some(id.as_str()))
                .and_then(assistant_text)
        })
        .collect();
    Some(TranscriptPage {
        next: TranscriptPosition::new(next),
        messages,
    })
}

fn read_prefix(path: &Path, len: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut prefix = Vec::new();
    let mut take: Take<File> = file.take(len);
    take.read_to_end(&mut prefix).ok()?;
    (prefix.len() as u64 == len).then_some(prefix)
}

fn project(folded: &FoldedTranscript) -> Vec<TranscriptMessage> {
    folded
        .messages
        .iter()
        .filter_map(|message| {
            let role = match message.kind.as_deref()? {
                "user" => TranscriptRole::User,
                "gemini" => TranscriptRole::Assistant,
                _ => return None,
            };
            let text = visible_text(message)?;
            let text = match role {
                TranscriptRole::User => sanitize_user_prompt(Some(&text))?,
                TranscriptRole::Assistant => text,
            };
            Some(TranscriptMessage {
                role,
                at: message
                    .timestamp
                    .as_deref()
                    .and_then(|value| value.parse::<Timestamp>().ok()),
                text,
            })
        })
        .collect()
}

fn assistant_text(message: &GeminiMessage) -> Option<String> {
    (message.kind.as_deref() == Some("gemini"))
        .then(|| visible_text(message))
        .flatten()
}

fn visible_text(message: &GeminiMessage) -> Option<String> {
    message
        .display_content
        .as_ref()
        .and_then(content_text)
        .or_else(|| message.content.as_ref().and_then(content_text))
}

fn content_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => object.get("text").and_then(content_text)?,
        Value::Array(parts) => parts
            .iter()
            .filter_map(content_text)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn projects_visible_conversation_shapes_and_sanitizes_users() {
        let parsed = messages(
            r#"{"id":"u","timestamp":"2026-06-02T10:00:00Z","type":"user","content":{"text":"  fix it  "}}
{"id":"a","timestamp":"2026-06-02T10:00:01Z","type":"gemini","content":"hidden","displayContent":[{"text":"done"},{"text":"now"}]}
{"id":"tool","type":"tool","content":"ignored"}
{"id":"empty","type":"gemini","content":{"toolCall":{"name":"read"}}}"#,
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].role, TranscriptRole::User);
        assert_eq!(parsed[0].text, "fix it");
        assert_eq!(parsed[1].text, "done\nnow");
        assert!(parsed.iter().all(|message| message.at.is_some()));
    }

    #[test]
    fn incremental_pages_suppress_updates_checkpoints_and_abandoned_branches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            b"{\"id\":\"old\",\"type\":\"gemini\",\"content\":\"old\"}\n",
        )
        .unwrap();
        let offset = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(
            b"{\"id\":\"old\",\"type\":\"gemini\",\"content\":\"detail update\"}\n\
{\"id\":\"abandoned\",\"type\":\"gemini\",\"content\":\"no\"}\n\
{\"$rewindTo\":\"abandoned\"}\n\
{\"id\":\"kept\",\"type\":\"gemini\",\"content\":\"yes\"}\n",
        )
        .unwrap();

        let page = assistant_page(&path, TranscriptPosition::new(offset)).unwrap();
        assert_eq!(page.messages, vec!["yes"]);
    }

    #[test]
    fn from_start_keeps_idless_assistants_but_incremental_pages_skip_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, b"{\"type\":\"gemini\",\"content\":\"first\"}\n").unwrap();
        let start = assistant_page(&path, TranscriptPosition::START).unwrap();
        assert_eq!(start.messages, vec!["first"]);
        let offset = start.next;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"gemini\",\"content\":\"second\"}\n")
            .unwrap();
        assert!(assistant_page(&path, offset).unwrap().messages.is_empty());
    }
}
