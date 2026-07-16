use std::path::Path;

use super::*;

use crate::agents::AgentStatus;
use crate::agents::lifecycle;
use crate::ids::{AgentKind, AgentSessionId, MessageId, WorkspaceId};
use crate::message::{DeliveryGate, MessageBody, MessageRecord, MessageStatus};
use crate::store::event::{EventEnvelope, MessageEventMethod};
use crate::store::snapshot::testkit::*;

fn recent(secs_ago: u64) -> jiff::Timestamp {
    jiff::Timestamp::now() - std::time::Duration::from_secs(secs_ago)
}

fn message_id(id: u64) -> MessageId {
    MessageId::parse(&format!("msg_{id:016x}")).expect("valid message id")
}

fn resume_event(
    workspace: &WorkspaceId,
    id: u64,
    agent_id: &str,
    status: MessageStatus,
    enqueued_at: jiff::Timestamp,
    updated_at: jiff::Timestamp,
) -> EventEnvelope {
    message_event(
        workspace,
        MessageEventFixture {
            id,
            agent_id,
            gate: DeliveryGate::Resume,
            body: MessageBody::Prompt,
            status,
            enqueued_at: Some(enqueued_at),
            updated_at,
        },
    )
}

struct MessageEventFixture<'a> {
    id: u64,
    agent_id: &'a str,
    gate: DeliveryGate,
    body: MessageBody,
    status: MessageStatus,
    enqueued_at: Option<jiff::Timestamp>,
    updated_at: jiff::Timestamp,
}

fn message_event(workspace: &WorkspaceId, fixture: MessageEventFixture<'_>) -> EventEnvelope {
    let mut message = MessageRecord::new(
        workspace.clone(),
        &agent("claude", fixture.agent_id, AgentStatus::Idle, 0),
        "continue".to_owned(),
        true,
        fixture.gate,
    );
    message.message_id = message_id(fixture.id);
    message.agent_id = AgentSessionId::from(fixture.agent_id);
    message.body = fixture.body;
    message.status = fixture.status;
    if let Some(enqueued_at) = fixture.enqueued_at {
        message.enqueued_at = enqueued_at;
    }
    message.updated_at = fixture.updated_at;
    let method = MessageEventMethod::for_terminal_status(fixture.status)
        .unwrap_or(MessageEventMethod::Queued);
    if fixture.enqueued_at.is_none() {
        let mut event = EventEnvelope::new(
            workspace.clone(),
            "session",
            "rimz",
            "cli",
            method.as_str(),
            serde_json::json!({
                "message_id": &message.message_id,
                "kind": &message.kind,
                "agent_id": &message.agent_id,
                "gate": fixture.gate,
                "status": fixture.status,
                "body": fixture.body,
                "forced": false,
                "text_len": message.text.len(),
                "enter": true,
                "attempts": 0,
            }),
        );
        event.timestamp = fixture.updated_at;
        return event;
    }
    let mut event = EventEnvelope::message_event(&message, "session", method, None);
    event.timestamp = fixture.updated_at;
    event
}

mod cache;
mod compact;
mod integrity;
mod merge;
mod resume;
mod rotation;
