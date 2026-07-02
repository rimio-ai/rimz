//! Provider-agnostic transcript message types.
//!
//! Adapters normalize their native JSONL into [`TranscriptMessage`]. Rimz-owned
//! chat timeline shaping lives in the transcript log and transcript CLI.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn answers_text(answers: &[AskAnswer]) -> String {
    answers
        .iter()
        .filter_map(|answer| {
            let mut line = answer
                .chosen
                .iter()
                .filter_map(|choice| non_empty(choice))
                .collect::<Vec<_>>()
                .join(", ");
            if line.is_empty() {
                return None;
            }
            if let Some(note) = answer.note.as_deref().and_then(non_empty) {
                line.push_str(" (note: ");
                line.push_str(note);
                line.push(')');
            }
            Some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChatEntry {
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<AskAnswer>,
}
