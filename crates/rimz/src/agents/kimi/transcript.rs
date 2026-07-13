//! Kimi Code durable conversation normalization.
//!
//! Human turns live in `turn.prompt`/`turn.steer`; normal assistant text is
//! reconstructed from recorded loop steps. Model-facing context and tool
//! plumbing stay out of the user-visible conversation.

use jiff::Timestamp;
use serde_json::Value;

use super::super::{TranscriptMessage, TranscriptRole, sanitize_user_prompt};
use super::wire::{self, WireRecord};

#[derive(Default)]
struct AssistantStep {
    id: String,
    text: Vec<String>,
    at: Option<Timestamp>,
}

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    let records = lines
        .lines()
        .filter_map(|line| serde_json::from_str::<WireRecord>(line).ok())
        .collect::<Vec<_>>();
    normalize(&records)
}

pub(super) fn normalize(records: &[WireRecord]) -> Vec<TranscriptMessage> {
    let mut messages = Vec::new();
    let mut steps = Vec::<AssistantStep>::new();

    for record in records {
        if let Some(prompt) = wire::prompt(record) {
            if prompt.origin.kind != "user" {
                continue;
            }
            flush_steps(&mut messages, &mut steps);
            let visible = prompt
                .input
                .iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| part.text.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(text) = sanitize_user_prompt(Some(&visible)) {
                messages.push(TranscriptMessage {
                    role: TranscriptRole::User,
                    at: record_time(record.time),
                    text,
                });
            }
            continue;
        }

        if let Some(event) = wire::loop_event(record) {
            match event.kind.as_str() {
                "step.begin" => {
                    let Some(id) = event.uuid else { continue };
                    if steps.iter().all(|step| step.id != id) {
                        steps.push(AssistantStep {
                            id,
                            at: record_time(record.time),
                            ..AssistantStep::default()
                        });
                    }
                }
                "content.part" => {
                    let Some(part) = event.part.filter(|part| part.kind == "text") else {
                        continue;
                    };
                    let Some(text) = part
                        .text
                        .map(|text| text.trim().to_owned())
                        .filter(|text| !text.is_empty())
                    else {
                        continue;
                    };
                    let Some(id) = event.step_uuid.or(event.uuid) else {
                        continue;
                    };
                    let step = if let Some(index) = steps.iter().position(|step| step.id == id) {
                        &mut steps[index]
                    } else {
                        let index = steps.len();
                        steps.push(AssistantStep {
                            id,
                            at: record_time(record.time),
                            ..AssistantStep::default()
                        });
                        // The just-pushed index is provably in bounds.
                        &mut steps[index]
                    };
                    step.at = record_time(record.time).or(step.at);
                    step.text.push(text);
                }
                "step.end" => {
                    let Some(id) = event.uuid else { continue };
                    if let Some(index) = steps.iter().position(|step| step.id == id) {
                        let mut step = steps.remove(index);
                        step.at = record_time(record.time).or(step.at);
                        emit_step(&mut messages, step);
                    }
                }
                _ => {}
            }
            continue;
        }

        if record.kind == "context.append_message"
            && let Some(message) = wire::record_message(record)
            && message.get("role").and_then(Value::as_str) == Some("assistant")
            && let Some(text) = visible_text(message.get("content"))
        {
            flush_steps(&mut messages, &mut steps);
            messages.push(TranscriptMessage {
                role: TranscriptRole::Assistant,
                at: record_time(record.time),
                text,
            });
        }
    }
    flush_steps(&mut messages, &mut steps);
    messages
}

pub(super) fn latest_assistant(lines: &str) -> Option<String> {
    let messages = parse_messages(lines);
    let latest_user = messages
        .iter()
        .rposition(|message| message.role == TranscriptRole::User);
    messages
        .iter()
        .enumerate()
        .rev()
        .find(|(index, message)| {
            message.role == TranscriptRole::Assistant
                && latest_user.is_none_or(|latest_user| *index > latest_user)
        })
        .map(|(_, message)| message.text.clone())
}

fn flush_steps(messages: &mut Vec<TranscriptMessage>, steps: &mut Vec<AssistantStep>) {
    for step in std::mem::take(steps) {
        emit_step(messages, step);
    }
}

fn emit_step(messages: &mut Vec<TranscriptMessage>, step: AssistantStep) {
    let text = step.text.join("\n");
    if !text.is_empty() {
        messages.push(TranscriptMessage {
            role: TranscriptRole::Assistant,
            at: step.at,
            text,
        });
    }
}

fn visible_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    let text = match content {
        Value::String(text) => text.trim().to_owned(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn record_time(time: Option<f64>) -> Option<Timestamp> {
    let millis = time?.trunc();
    if millis > i64::MAX as f64 {
        return None;
    }
    Timestamp::from_millisecond(millis as i64).ok()
}
