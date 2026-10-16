//! OMP visible conversation projection.

use crate::agents::{TranscriptMessage, TranscriptRole, sanitize_user_prompt};
use serde_json::Value;

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    lines
        .lines()
        .filter_map(|line| {
            let entry = serde_json::from_str::<Value>(line).ok()?;
            if entry.get("type").and_then(Value::as_str) != Some("message") {
                return None;
            }
            let message = entry.get("message")?;
            let role = match message.get("role").and_then(Value::as_str)? {
                "user" => TranscriptRole::User,
                "assistant" => TranscriptRole::Assistant,
                _ => return None,
            };
            let content = message.get("content")?;
            let visible = if let Some(text) = content.as_str() {
                text.trim().to_owned()
            } else {
                content
                    .as_array()?
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if visible.is_empty() {
                return None;
            }
            let text = if role == TranscriptRole::User {
                sanitize_user_prompt(Some(&visible))?
            } else {
                visible
            };
            Some(TranscriptMessage {
                role,
                text,
                at: entry
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok()),
            })
        })
        .collect()
}
