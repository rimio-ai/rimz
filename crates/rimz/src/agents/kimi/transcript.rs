//! Kimi Code durable conversation normalization.
//!
//! Human turns and assistant steps arrive as typed Wire events. Model-facing
//! context, thinking, and tool plumbing stay out of visible conversation.

use jiff::Timestamp;

use super::super::{TranscriptMessage, TranscriptRole, sanitize_user_prompt};
use super::wire::{
    self, AppendedMessage, ContentPart, LoopEvent, MessageContent, MessageRole, PromptOrigin,
    PromptRecord, WireEvent, WireRecord,
};

#[derive(Default)]
struct AssistantStep {
    id: String,
    text: Vec<String>,
    at: Option<Timestamp>,
}

#[derive(Default)]
struct TranscriptFold {
    messages: Vec<TranscriptMessage>,
    steps: Vec<AssistantStep>,
}

impl TranscriptFold {
    fn observe(&mut self, record: &WireRecord) {
        match &record.event {
            WireEvent::Prompt { prompt, .. } if prompt.origin == PromptOrigin::User => {
                self.observe_prompt(record, prompt);
            }
            WireEvent::AppendLoopEvent(event) => self.observe_loop_event(record, event),
            WireEvent::AppendMessage(message) if message.role == MessageRole::Assistant => {
                self.observe_message(record, message);
            }
            _ => {}
        }
    }

    fn observe_prompt(&mut self, record: &WireRecord, prompt: &PromptRecord) {
        self.flush_steps();
        let visible = visible_parts(&prompt.input);
        if let Some(text) = sanitize_user_prompt(Some(&visible)) {
            self.messages.push(TranscriptMessage {
                role: TranscriptRole::User,
                at: record.timestamp(),
                text,
            });
        }
    }

    fn observe_loop_event(&mut self, record: &WireRecord, event: &LoopEvent) {
        match event {
            LoopEvent::StepBegin { id: Some(id) } => self.begin_step(id, record.timestamp()),
            LoopEvent::ContentPart {
                step_id: Some(id),
                part: ContentPart::Text(text),
            } => self.append_part(id, text, record.timestamp()),
            LoopEvent::StepEnd { id: Some(id), .. } => self.end_step(id, record.timestamp()),
            _ => {}
        }
    }

    fn observe_message(&mut self, record: &WireRecord, message: &AppendedMessage) {
        let Some(text) = visible_message(&message.content) else {
            return;
        };
        self.flush_steps();
        self.messages.push(TranscriptMessage {
            role: TranscriptRole::Assistant,
            at: record.timestamp(),
            text,
        });
    }

    fn begin_step(&mut self, id: &str, at: Option<Timestamp>) {
        if self.steps.iter().all(|step| step.id != id) {
            self.steps.push(AssistantStep {
                id: id.to_owned(),
                at,
                ..AssistantStep::default()
            });
        }
    }

    fn append_part(&mut self, id: &str, text: &str, at: Option<Timestamp>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let step = self.step_mut(id, at);
        step.at = at.or(step.at);
        step.text.push(text.to_owned());
    }

    fn step_mut(&mut self, id: &str, at: Option<Timestamp>) -> &mut AssistantStep {
        if let Some(index) = self.steps.iter().position(|step| step.id == id) {
            return &mut self.steps[index];
        }
        self.steps.push(AssistantStep {
            id: id.to_owned(),
            at,
            ..AssistantStep::default()
        });
        let index = self.steps.len() - 1;
        &mut self.steps[index]
    }

    fn end_step(&mut self, id: &str, at: Option<Timestamp>) {
        let Some(index) = self.steps.iter().position(|step| step.id == id) else {
            return;
        };
        let mut step = self.steps.remove(index);
        step.at = at.or(step.at);
        emit_step(&mut self.messages, step);
    }

    fn flush_steps(&mut self) {
        for step in std::mem::take(&mut self.steps) {
            emit_step(&mut self.messages, step);
        }
    }

    fn finish(mut self) -> Vec<TranscriptMessage> {
        self.flush_steps();
        self.messages
    }
}

pub(super) fn parse_messages(lines: &str) -> Vec<TranscriptMessage> {
    normalize(&wire::records_from_str(lines))
}

pub(super) fn normalize(records: &[WireRecord]) -> Vec<TranscriptMessage> {
    let mut fold = TranscriptFold::default();
    for record in records {
        fold.observe(record);
    }
    fold.finish()
}

pub(super) fn latest_assistant(lines: &str) -> Option<String> {
    latest_assistant_from_records(&wire::records_from_str(lines))
}

pub(super) fn latest_assistant_from_records(records: &[WireRecord]) -> Option<String> {
    let messages = normalize(records);
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

fn visible_parts(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(ContentPart::text)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn visible_message(content: &MessageContent) -> Option<String> {
    let text = match content {
        MessageContent::Text(text) => text.trim().to_owned(),
        MessageContent::Parts(parts) => visible_parts(parts),
        MessageContent::Other => return None,
    };
    (!text.is_empty()).then_some(text)
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
