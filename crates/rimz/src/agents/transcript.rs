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
    pub options: Vec<AskOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "AskOptionWire", into = "AskOptionWire")]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AskOptionWire {
    Label(String),
    Detailed {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl From<AskOptionWire> for AskOption {
    fn from(value: AskOptionWire) -> Self {
        match value {
            AskOptionWire::Label(label) => Self {
                label,
                description: None,
            },
            AskOptionWire::Detailed { label, description } => Self { label, description },
        }
    }
}

impl From<AskOption> for AskOptionWire {
    fn from(value: AskOption) -> Self {
        match value.description {
            Some(description) => AskOptionWire::Detailed {
                label: value.label,
                description: Some(description),
            },
            None => AskOptionWire::Label(value.label),
        }
    }
}

impl From<String> for AskOption {
    fn from(label: String) -> Self {
        Self {
            label,
            description: None,
        }
    }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatEntry {
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    pub text: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<AskQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<AskAnswer>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ask_option_deserializes_legacy_string_shape() {
        let option: AskOption = serde_json::from_value(json!("safe")).expect("decode option");

        assert_eq!(
            option,
            AskOption {
                label: "safe".to_owned(),
                description: None,
            }
        );
    }

    #[test]
    fn ask_option_deserializes_object_shape() {
        let option: AskOption =
            serde_json::from_value(json!({"label": "safe", "description": "Use staged rollout"}))
                .expect("decode option");

        assert_eq!(
            option,
            AskOption {
                label: "safe".to_owned(),
                description: Some("Use staged rollout".to_owned()),
            }
        );
    }

    #[test]
    fn ask_option_deserializes_object_without_description() {
        let option: AskOption =
            serde_json::from_value(json!({"label": "safe"})).expect("decode option");

        assert_eq!(option, AskOption::from("safe".to_owned()));
    }

    #[test]
    fn ask_option_serializes_label_only_as_string_and_description_as_object() {
        assert_eq!(
            serde_json::to_value(AskOption::from("safe".to_owned())).expect("serialize option"),
            json!("safe")
        );
        assert_eq!(
            serde_json::to_value(AskOption {
                label: "safe".to_owned(),
                description: Some("Use staged rollout".to_owned()),
            })
            .expect("serialize option"),
            json!({"label": "safe", "description": "Use staged rollout"})
        );
    }
}
